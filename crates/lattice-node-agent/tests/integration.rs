//! Integration tests for lattice-node-agent.
//!
//! These tests exercise multi-component interactions across module
//! boundaries: prologue + runtime + cache, epilogue + runtime + sensitive
//! wipe, full runtime lifecycle, agent heartbeat + health, signal
//! delivery through all modes, and image cache eviction.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use lattice_common::types::NodeCapabilities;
use lattice_node_agent::agent::{AgentCommand, NodeAgent};
use lattice_node_agent::cgroup::StubCgroupManager;
use lattice_node_agent::checkpoint_handler::{CheckpointHandler, CheckpointMode};
use lattice_node_agent::conformance::ConformanceComponents;
use lattice_node_agent::data_stage::NoopDataStageExecutor;
use lattice_node_agent::epilogue::{
    EpilogueConfig, EpiloguePipeline, NoopEpilogueReporter, NoopSensitiveWiper, SensitiveWiper,
};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::heartbeat_loop::{HeartbeatSink, StaticHealthObserver};
use lattice_node_agent::image_cache::ImageCache;
use lattice_node_agent::prologue::{NoopReporter, ProloguePipeline};
use lattice_node_agent::runtime::{ExitStatus, MockRuntime, PrepareConfig, Runtime};
use lattice_node_agent::signal::{
    DeliveryResult, GrpcCheckpointClient, NoopGrpcClient, NoopShmemWriter, ShmemWriter,
    SignalDelivery,
};
use lattice_node_agent::telemetry::log_buffer::{LogRingBuffer, S3Sink};
use lattice_test_harness::fixtures::create_node_batch;
use lattice_test_harness::mocks::MockNodeRegistry;

// ---------------------------------------------------------------
// Shared test helpers
// ---------------------------------------------------------------

fn test_capabilities() -> NodeCapabilities {
    NodeCapabilities {
        gpu_type: Some("GH200".to_string()),
        gpu_count: 4,
        cpu_cores: 72,
        memory_gb: 512,
        features: vec![],
        gpu_topology: None,
        memory_topology: None,
    }
}

fn healthy_observed() -> ObservedHealth {
    ObservedHealth {
        gpu_count: 4,
        max_gpu_temp_c: Some(65.0),
        ecc_errors: 0,
        nic_up: true,
    }
}

fn make_agent() -> NodeAgent {
    let nodes = create_node_batch(1, 0);
    let node_id = nodes[0].id.clone();
    let registry = Arc::new(MockNodeRegistry::new().with_nodes(nodes));
    NodeAgent::new(node_id, test_capabilities(), registry)
}

type UploadLog = Vec<(String, String, Vec<u8>)>;
type AllocIdLog = Vec<lattice_common::types::AllocId>;
type GrpcCallLog = Vec<(String, lattice_common::types::AllocId)>;

/// Mock S3 sink that records uploads.
struct MockS3 {
    uploads: Arc<Mutex<UploadLog>>,
}

impl MockS3 {
    fn new() -> (Self, Arc<Mutex<UploadLog>>) {
        let uploads = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                uploads: uploads.clone(),
            },
            uploads,
        )
    }
}

#[async_trait]
impl S3Sink for MockS3 {
    async fn upload(&self, bucket: &str, key: &str, data: Vec<u8>) -> Result<(), String> {
        self.uploads
            .lock()
            .await
            .push((bucket.to_string(), key.to_string(), data));
        Ok(())
    }
}

/// Mock sensitive wiper that records wipe calls.
struct RecordingSensitiveWiper {
    wiped: Arc<Mutex<AllocIdLog>>,
}

impl RecordingSensitiveWiper {
    fn new() -> (Self, Arc<Mutex<AllocIdLog>>) {
        let wiped = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                wiped: wiped.clone(),
            },
            wiped,
        )
    }
}

#[async_trait]
impl SensitiveWiper for RecordingSensitiveWiper {
    async fn wipe(&self, alloc_id: lattice_common::types::AllocId) -> Result<(), String> {
        self.wiped.lock().await.push(alloc_id);
        Ok(())
    }
}

/// Recording shmem writer for signal delivery tests.
struct RecordingShmemWriter {
    flags_written: Arc<Mutex<AllocIdLog>>,
}

impl RecordingShmemWriter {
    fn new() -> (Self, Arc<Mutex<AllocIdLog>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                flags_written: written.clone(),
            },
            written,
        )
    }
}

#[async_trait]
impl ShmemWriter for RecordingShmemWriter {
    async fn write_checkpoint_flag(
        &self,
        alloc_id: lattice_common::types::AllocId,
    ) -> Result<(), String> {
        self.flags_written.lock().await.push(alloc_id);
        Ok(())
    }
    async fn clear_checkpoint_flag(
        &self,
        _alloc_id: lattice_common::types::AllocId,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Recording gRPC client for signal delivery tests.
struct RecordingGrpcClient {
    calls: Arc<Mutex<GrpcCallLog>>,
}

impl RecordingGrpcClient {
    fn new() -> (Self, Arc<Mutex<GrpcCallLog>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl GrpcCheckpointClient for RecordingGrpcClient {
    async fn notify_checkpoint(
        &self,
        endpoint: &str,
        alloc_id: lattice_common::types::AllocId,
    ) -> Result<(), String> {
        self.calls
            .lock()
            .await
            .push((endpoint.to_string(), alloc_id));
        Ok(())
    }
}

/// Mock heartbeat sink that records heartbeats.
struct RecordingSink {
    heartbeats: Arc<std::sync::Mutex<Vec<lattice_node_agent::heartbeat::Heartbeat>>>,
}

impl RecordingSink {
    fn new() -> (
        Self,
        Arc<std::sync::Mutex<Vec<lattice_node_agent::heartbeat::Heartbeat>>>,
    ) {
        let store = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                heartbeats: store.clone(),
            },
            store,
        )
    }
}

#[async_trait]
impl HeartbeatSink for RecordingSink {
    async fn send(
        &self,
        heartbeat: lattice_node_agent::heartbeat::Heartbeat,
    ) -> Result<(), String> {
        self.heartbeats.lock().unwrap().push(heartbeat);
        Ok(())
    }
}

// ===============================================================
// Test 1: Prologue + Runtime + Cache -- cache miss then cache hit
// ===============================================================

#[tokio::test]
async fn prologue_runtime_cache_miss_then_hit() {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

    let alloc_id_1 = Uuid::new_v4();
    let config_1 = PrepareConfig {
        alloc_id: alloc_id_1,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: Some("default".to_string()),
        image: None,
        workdir: Some("/workspace".to_string()),
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };

    // First prologue run: cache miss, runtime.prepare called, cache updated.
    let result_1 = pipeline
        .execute(
            alloc_id_1,
            &config_1,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();

    assert!(!result_1.cache_hit, "first run should be a cache miss");
    assert!(
        cache.contains("prgenv-gnu/24.11:v1"),
        "image should now be cached"
    );

    // Verify runtime.prepare was called for the first allocation.
    let calls_1 = runtime.calls_for(alloc_id_1).await;
    assert_eq!(calls_1.len(), 1, "runtime.prepare should be called once");

    // Second prologue run with a different alloc but the same image: cache hit.
    let alloc_id_2 = Uuid::new_v4();
    let config_2 = PrepareConfig {
        alloc_id: alloc_id_2,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: Some("default".to_string()),
        image: None,
        workdir: Some("/workspace".to_string()),
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };

    let result_2 = pipeline
        .execute(
            alloc_id_2,
            &config_2,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();

    assert!(result_2.cache_hit, "second run should be a cache hit");

    // runtime.prepare is still called (prepare is always needed even with a cache hit).
    let calls_2 = runtime.calls_for(alloc_id_2).await;
    assert_eq!(
        calls_2.len(),
        1,
        "runtime.prepare called for second alloc too"
    );

    // Total runtime calls: 2 prepares.
    assert_eq!(runtime.call_count().await, 2);
}

// ===============================================================
// Test 2: Epilogue + Runtime + Sensitive wipe
// ===============================================================

#[tokio::test]
async fn epilogue_runtime_sensitive_wipe() {
    let alloc_id = Uuid::new_v4();

    // Prepare the runtime so cleanup will succeed.
    let runtime = MockRuntime::new();
    let prep_config = PrepareConfig {
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
    runtime.prepare(&prep_config).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let (wiper, wiped_ids) = RecordingSensitiveWiper::new();

    // Epilogue with sensitive_wipe = true.
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
            None, // no S3
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &wiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    // Sensitive wipe was triggered.
    assert!(
        result.sensitive_wiped,
        "sensitive wipe should have occurred"
    );
    let wiped = wiped_ids.lock().await;
    assert_eq!(wiped.len(), 1);
    assert_eq!(wiped[0], alloc_id);

    // Runtime cleanup succeeded.
    assert!(result.cleaned_up, "runtime cleanup should succeed");

    // Exit status preserved.
    assert_eq!(result.exit_status, ExitStatus::Code(0));
}

// ===============================================================
// Test 3: Full runtime lifecycle -- Prepare -> Spawn -> Signal -> Stop -> Cleanup
// ===============================================================

#[tokio::test]
async fn runtime_full_lifecycle() {
    let runtime = MockRuntime::new();
    let alloc_id = Uuid::new_v4();

    // Phase 1: Prepare
    let config = PrepareConfig {
        alloc_id,
        uenv: Some("pytorch/2.3:v1".to_string()),
        view: Some("default".to_string()),
        image: None,
        workdir: Some("/workspace".to_string()),
        env_vars: vec![
            ("CUDA_VISIBLE_DEVICES".to_string(), "0,1,2,3".to_string()),
            ("MASTER_PORT".to_string(), "29500".to_string()),
        ],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    runtime.prepare(&config).await.unwrap();

    // Phase 2: Spawn
    let handle = runtime
        .spawn(alloc_id, "python", &["train.py".to_string()])
        .await
        .unwrap();
    assert!(handle.pid.is_some());
    assert!(handle.container_id.is_some());

    // Phase 3: Signal (checkpoint SIGUSR1)
    runtime.signal(&handle, 10).await.unwrap();

    // Phase 4: Wait (process still running, returns configured exit)
    let wait_status = runtime.wait(&handle).await.unwrap();
    assert_eq!(wait_status, ExitStatus::Code(0));

    // Phase 5: Stop
    let stop_status = runtime.stop(&handle, 30).await.unwrap();
    assert_eq!(stop_status, ExitStatus::Signal(15));

    // Phase 6: Cleanup
    runtime.cleanup(alloc_id).await.unwrap();

    // Verify all 6 lifecycle phases were called in order.
    let calls = runtime.calls().await;
    assert_eq!(calls.len(), 6);

    // Check that calls for this allocation match the expected pattern.
    let alloc_calls = runtime.calls_for(alloc_id).await;
    assert_eq!(alloc_calls.len(), 6);
}

// ===============================================================
// Test 4: Agent heartbeat + health -- generate heartbeats,
// verify sequences increment and health state is reflected
// ===============================================================

#[tokio::test]
async fn agent_heartbeat_health_integration() {
    let mut agent = make_agent();

    // Compute conformance so heartbeats carry a fingerprint.
    agent.compute_conformance(&ConformanceComponents {
        gpu_driver_version: "535.129.03".to_string(),
        nic_firmware_version: "22.39.1002".to_string(),
        bios_version: "2.7.1".to_string(),
        bmc_firmware_version: "13.1".to_string(),
        kernel_version: "6.1.0-cray".to_string(),
        kernel_parameters: vec!["iommu=pt".to_string()],
    });

    // Run a health check.
    let health = agent.check_health(&healthy_observed());
    assert!(health.healthy());
    assert!(health.issues().is_empty());

    // Generate multiple heartbeats and verify sequences increment.
    let hb1 = agent.generate_heartbeat(&healthy_observed());
    let hb2 = agent.generate_heartbeat(&healthy_observed());
    let hb3 = agent.generate_heartbeat(&healthy_observed());

    assert_eq!(hb1.sequence, 1);
    assert_eq!(hb2.sequence, 2);
    assert_eq!(hb3.sequence, 3);
    assert!(hb1.healthy);
    assert!(hb1.conformance_fingerprint.is_some());
    assert_eq!(hb1.running_allocations, 0);

    // Start some allocations to affect heartbeat payload.
    agent
        .handle_command(AgentCommand::StartAllocation {
            id: Uuid::new_v4(),
            entrypoint: "train.py".to_string(),
            liveness_probe: None,
        })
        .await
        .unwrap();

    let hb4 = agent.generate_heartbeat(&healthy_observed());
    assert_eq!(hb4.sequence, 4);
    assert_eq!(hb4.running_allocations, 1);

    // Now check with unhealthy observations.
    let unhealthy = ObservedHealth {
        gpu_count: 2, // expected 4
        max_gpu_temp_c: Some(95.0),
        ecc_errors: 0,
        nic_up: true,
    };
    let hb5 = agent.generate_heartbeat(&unhealthy);
    assert!(!hb5.healthy);
    assert!(!hb5.issues.is_empty());
}

// ===============================================================
// Test 5: Signal delivery through all three modes
// ===============================================================

#[tokio::test]
async fn signal_delivery_all_modes() {
    // -- Mode 1: Signal (SIGUSR1) --
    let alloc_signal = Uuid::new_v4();
    let rt_signal = MockRuntime::new();
    let prep = PrepareConfig {
        alloc_id: alloc_signal,
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
    rt_signal.prepare(&prep).await.unwrap();
    let handle_signal = rt_signal.spawn(alloc_signal, "python", &[]).await.unwrap();

    let mut handler_signal = CheckpointHandler::new();
    handler_signal.request_checkpoint(alloc_signal, CheckpointMode::Signal);

    let result_signal = SignalDelivery::deliver(
        &mut handler_signal,
        alloc_signal,
        &handle_signal,
        &rt_signal,
        &NoopShmemWriter,
        &NoopGrpcClient,
    )
    .await;
    assert_eq!(result_signal, DeliveryResult::Delivered);
    assert_eq!(handler_signal.completed_count(&alloc_signal), 1);

    // -- Mode 2: Shared Memory --
    let alloc_shmem = Uuid::new_v4();
    let rt_shmem = MockRuntime::new();
    let prep2 = PrepareConfig {
        alloc_id: alloc_shmem,
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
    rt_shmem.prepare(&prep2).await.unwrap();
    let handle_shmem = rt_shmem.spawn(alloc_shmem, "app", &[]).await.unwrap();

    let (shmem_writer, shmem_written) = RecordingShmemWriter::new();
    let mut handler_shmem = CheckpointHandler::new();
    handler_shmem.request_checkpoint(alloc_shmem, CheckpointMode::SharedMemory);

    let result_shmem = SignalDelivery::deliver(
        &mut handler_shmem,
        alloc_shmem,
        &handle_shmem,
        &rt_shmem,
        &shmem_writer,
        &NoopGrpcClient,
    )
    .await;
    assert_eq!(result_shmem, DeliveryResult::Delivered);
    let written = shmem_written.lock().await;
    assert_eq!(written.len(), 1);
    assert_eq!(written[0], alloc_shmem);

    // -- Mode 3: gRPC Callback --
    let alloc_grpc = Uuid::new_v4();
    let rt_grpc = MockRuntime::new();
    let prep3 = PrepareConfig {
        alloc_id: alloc_grpc,
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
    rt_grpc.prepare(&prep3).await.unwrap();
    let handle_grpc = rt_grpc.spawn(alloc_grpc, "server", &[]).await.unwrap();

    let (grpc_client, grpc_calls) = RecordingGrpcClient::new();
    let mut handler_grpc = CheckpointHandler::new();
    handler_grpc.request_checkpoint(
        alloc_grpc,
        CheckpointMode::GrpcCallback {
            endpoint: "localhost:9090".to_string(),
        },
    );

    let result_grpc = SignalDelivery::deliver(
        &mut handler_grpc,
        alloc_grpc,
        &handle_grpc,
        &rt_grpc,
        &NoopShmemWriter,
        &grpc_client,
    )
    .await;
    assert_eq!(result_grpc, DeliveryResult::Delivered);
    let calls = grpc_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "localhost:9090");
    assert_eq!(calls[0].1, alloc_grpc);
}

// ===============================================================
// Test 6: Image cache eviction -- fill to capacity, verify LRU
// ===============================================================

#[tokio::test]
async fn image_cache_lru_eviction() {
    // Cache fits 3 images of 100 bytes each (capacity 300).
    let mut cache = ImageCache::new(300);

    // Insert image-a, image-b, image-c -- fills cache exactly.
    cache.insert("image-a".to_string(), 100);
    cache.insert("image-b".to_string(), 100);
    cache.insert("image-c".to_string(), 100);

    assert_eq!(cache.len(), 3);
    assert_eq!(cache.current_bytes(), 300);

    // Touch image-a to make it recently used.
    assert!(cache.touch("image-a"));

    // Insert image-d: should evict image-b (the LRU, since image-a was touched).
    let evicted = cache.insert("image-d".to_string(), 100);
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0], "image-b");
    assert!(!cache.contains("image-b"));
    assert!(cache.contains("image-a"));
    assert!(cache.contains("image-c"));
    assert!(cache.contains("image-d"));

    // Insert a large image that requires evicting two entries.
    let evicted_multi = cache.insert("image-large".to_string(), 200);
    assert_eq!(evicted_multi.len(), 2);
    // image-c and one of (image-a, image-d) should be evicted.
    assert_eq!(cache.len(), 2);
    assert!(cache.contains("image-large"));

    // Verify total bytes remain consistent.
    assert!(cache.current_bytes() <= cache.max_bytes());
}

// ===============================================================
// Test 7: Epilogue with log flush + runtime cleanup + no sensitive wipe
// ===============================================================

#[tokio::test]
async fn epilogue_log_flush_and_cleanup_no_sensitive() {
    let alloc_id = Uuid::new_v4();

    // Prepare the runtime so cleanup succeeds.
    let runtime = MockRuntime::new();
    let prep_config = PrepareConfig {
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
    runtime.prepare(&prep_config).await.unwrap();

    // Write some log data into the ring buffer.
    let mut log_buf = LogRingBuffer::with_capacity(4096);
    log_buf.write(b"epoch 1/100 loss=2.31\n");
    log_buf.write(b"epoch 2/100 loss=1.85\n");

    let (s3, uploads) = MockS3::new();

    let pipeline = EpiloguePipeline::default();
    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            Some(&s3),
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &NoopSensitiveWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    // Logs should be flushed to S3.
    assert!(result.logs_flushed);
    let uploads = uploads.lock().await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].0, "lattice-logs");
    assert!(uploads[0].1.contains(&alloc_id.to_string()));
    let log_content = String::from_utf8(uploads[0].2.clone()).unwrap();
    assert!(log_content.contains("epoch 1/100"));
    assert!(log_content.contains("epoch 2/100"));

    // Runtime cleanup completed.
    assert!(result.cleaned_up);

    // No sensitive wipe (default config has sensitive_wipe=false).
    assert!(!result.sensitive_wiped);

    // Exit status passed through.
    assert_eq!(result.exit_status, ExitStatus::Code(0));
}

// ===============================================================
// Test 8: Agent run loop -- heartbeats + commands integration
// ===============================================================

#[tokio::test]
async fn agent_run_loop_heartbeats_and_commands() {
    let mut agent = make_agent();
    let (sink, store) = RecordingSink::new();
    let observer = StaticHealthObserver::new(healthy_observed());
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(16);

    let alloc_1 = Uuid::new_v4();
    let alloc_2 = Uuid::new_v4();

    // Spawn a task that sends commands and then cancels.
    tokio::spawn(async move {
        // Give the loop time to start and send an initial heartbeat.
        tokio::time::sleep(Duration::from_millis(30)).await;

        // Start two allocations.
        cmd_tx
            .send(AgentCommand::StartAllocation {
                id: alloc_1,
                entrypoint: "train.py".to_string(),
                liveness_probe: None,
            })
            .await
            .unwrap();

        cmd_tx
            .send(AgentCommand::StartAllocation {
                id: alloc_2,
                entrypoint: "eval.py".to_string(),
                liveness_probe: None,
            })
            .await
            .unwrap();

        // Let a few more heartbeats fire.
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Stop one allocation.
        cmd_tx
            .send(AgentCommand::StopAllocation { id: alloc_1 })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = cancel_tx.send(true);
    });

    agent
        .run(sink, observer, Duration::from_millis(15), cancel_rx, cmd_rx)
        .await;

    // Verify heartbeats were sent.
    let hbs = store.lock().unwrap();
    assert!(!hbs.is_empty(), "heartbeats should have been sent");

    // Sequences should be strictly increasing.
    for window in hbs.windows(2) {
        assert!(
            window[1].sequence > window[0].sequence,
            "heartbeat sequences must be strictly increasing"
        );
    }

    // Commands were processed: alloc_1 stopped, alloc_2 still running.
    assert_eq!(
        agent.active_allocation_count(),
        1,
        "one allocation should remain active"
    );
}

// ===============================================================
// Test 9: Prologue + epilogue end-to-end across modules
// ===============================================================

#[tokio::test]
async fn prologue_then_epilogue_end_to_end() {
    let alloc_id = Uuid::new_v4();
    let runtime = MockRuntime::new();
    let mut cache = ImageCache::new(10 * 1024 * 1024 * 1024);

    // -- Prologue phase --
    let prep_config = PrepareConfig {
        alloc_id,
        uenv: Some("ml-tools/2.0:v1".to_string()),
        view: Some("default".to_string()),
        image: None,
        workdir: Some("/home/user".to_string()),
        env_vars: vec![("DATA_DIR".to_string(), "/data".to_string())],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };

    let prologue = ProloguePipeline::default();
    let prologue_result = prologue
        .execute(
            alloc_id,
            &prep_config,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();

    assert!(!prologue_result.cache_hit);
    assert!(!prologue_result.data_staged); // no data_mounts configured
    assert!(cache.contains("ml-tools/2.0:v1"));

    // -- Simulate running: spawn, do some work, write logs --
    let handle = runtime
        .spawn(alloc_id, "python", &["train.py".to_string()])
        .await
        .unwrap();

    let mut log_buf = LogRingBuffer::with_capacity(2048);
    log_buf.write(b"training started\n");
    log_buf.write(b"training completed\n");

    let exit_status = runtime.wait(&handle).await.unwrap();
    assert!(exit_status.success());

    // -- Epilogue phase --
    let (s3, uploads) = MockS3::new();

    let epilogue = EpiloguePipeline::default();
    let epilogue_result = epilogue
        .execute(
            alloc_id,
            exit_status,
            &runtime,
            &log_buf,
            Some(&s3),
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &NoopSensitiveWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    assert!(epilogue_result.logs_flushed);
    assert!(epilogue_result.cleaned_up);
    assert!(!epilogue_result.sensitive_wiped);
    assert_eq!(epilogue_result.exit_status, ExitStatus::Code(0));

    // Verify the log data was flushed to S3.
    let uploads = uploads.lock().await;
    assert_eq!(uploads.len(), 1);
    let flushed_log = String::from_utf8(uploads[0].2.clone()).unwrap();
    assert!(flushed_log.contains("training started"));
    assert!(flushed_log.contains("training completed"));

    // Verify runtime was called in the correct order across phases.
    let all_calls = runtime.calls_for(alloc_id).await;
    // prepare (prologue) + spawn + wait + cleanup (epilogue) = 4 calls
    assert_eq!(all_calls.len(), 4);
}

// ===============================================================
// Test 10: Agent checkpoint command -> signal delivery pipeline
// ===============================================================

#[tokio::test]
async fn agent_checkpoint_to_signal_delivery() {
    let mut agent = make_agent();
    let alloc_id = Uuid::new_v4();

    // Start an allocation via the agent.
    agent
        .handle_command(AgentCommand::StartAllocation {
            id: alloc_id,
            entrypoint: "server".to_string(),
            liveness_probe: None,
        })
        .await
        .unwrap();

    assert_eq!(agent.active_allocation_count(), 1);

    // Issue a checkpoint command through the agent.
    agent
        .handle_command(AgentCommand::Checkpoint { id: alloc_id })
        .await
        .unwrap();

    // Verify the checkpoint is pending in the handler.
    let pending = agent.checkpoints().pending_for(&alloc_id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].mode, CheckpointMode::Signal);

    // Set up a runtime and deliver the signal.
    let runtime = MockRuntime::new();
    let prep = PrepareConfig {
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
    runtime.prepare(&prep).await.unwrap();
    let handle = runtime.spawn(alloc_id, "server", &[]).await.unwrap();

    // Create a mutable handler that mirrors the agent's handler state.
    let mut delivery_handler = CheckpointHandler::new();
    delivery_handler.request_checkpoint(alloc_id, CheckpointMode::Signal);

    let result = SignalDelivery::deliver(
        &mut delivery_handler,
        alloc_id,
        &handle,
        &runtime,
        &NoopShmemWriter,
        &NoopGrpcClient,
    )
    .await;

    assert_eq!(result, DeliveryResult::Delivered);
    assert_eq!(delivery_handler.completed_count(&alloc_id), 1);

    // Verify the runtime received the SIGUSR1 signal.
    let runtime_calls = runtime.calls_for(alloc_id).await;
    let signal_calls: Vec<_> = runtime_calls
        .iter()
        .filter(|c| {
            matches!(
                c,
                lattice_node_agent::runtime::mock::MockCall::Signal { .. }
            )
        })
        .collect();
    assert_eq!(signal_calls.len(), 1);
}

// ===============================================================
// Test 11: Attach session lifecycle -- attach, write, read, resize, detach
// ===============================================================

#[tokio::test]
async fn attach_session_lifecycle() {
    use lattice_node_agent::allocation_runner::AllocationManager;
    use lattice_node_agent::attach::{AttachManager, MockOwnerLookup};
    use lattice_node_agent::pty::{MockPtyBackend, TerminalSize};

    let alloc_id = Uuid::new_v4();
    let user = "alice".to_string();

    // Set up allocation manager with a running allocation.
    let alloc_mgr = {
        let mut mgr = AllocationManager::new();
        mgr.start(alloc_id, "python train.py".to_string()).unwrap();
        mgr.advance(&alloc_id).unwrap(); // Prologue -> Running
        Arc::new(Mutex::new(mgr))
    };

    let owner_lookup = Arc::new(MockOwnerLookup::new());
    owner_lookup.set_owner(alloc_id, user.clone()).await;

    let pty_backend = Arc::new(MockPtyBackend::new());
    let attach_mgr = AttachManager::new(pty_backend.clone(), alloc_mgr, owner_lookup);

    // Attach
    let session = attach_mgr
        .attach(alloc_id, user.clone(), None, TerminalSize::default())
        .await
        .unwrap();
    assert_eq!(session.alloc_id, alloc_id);
    assert_eq!(session.user, user);
    assert_eq!(attach_mgr.active_session_count().await, 1);

    // Write user input
    attach_mgr
        .write(&session.id, b"echo hello\n")
        .await
        .unwrap();
    let written = pty_backend.written_data(&session.pty_session_id).await;
    assert_eq!(written, b"echo hello\n");

    // Enqueue process output and read
    pty_backend
        .enqueue_read_data(&session.pty_session_id, b"hello\n".to_vec())
        .await;
    let output = attach_mgr.read(&session.id).await.unwrap();
    assert_eq!(output, b"hello\n");

    // Resize terminal
    let new_size = TerminalSize {
        rows: 40,
        cols: 160,
    };
    attach_mgr.resize(&session.id, new_size).await.unwrap();
    let actual_size = pty_backend
        .current_size(&session.pty_session_id)
        .await
        .unwrap();
    assert_eq!(actual_size, new_size);

    // Detach
    let exit_code = attach_mgr.detach(&session.id).await.unwrap();
    assert_eq!(exit_code, None);
    assert_eq!(attach_mgr.active_session_count().await, 0);

    // Session is gone
    assert!(attach_mgr.get_session(&session.id).await.is_none());
}

// ===============================================================
// Test 12: Attach permission denied for wrong user
// ===============================================================

#[tokio::test]
async fn attach_permission_denied() {
    use lattice_node_agent::allocation_runner::AllocationManager;
    use lattice_node_agent::attach::{AttachError, AttachManager, MockOwnerLookup};
    use lattice_node_agent::pty::{MockPtyBackend, TerminalSize};

    let alloc_id = Uuid::new_v4();
    let owner = "alice".to_string();
    let intruder = "eve".to_string();

    let alloc_mgr = {
        let mut mgr = AllocationManager::new();
        mgr.start(alloc_id, "train.py".to_string()).unwrap();
        mgr.advance(&alloc_id).unwrap(); // Prologue -> Running
        Arc::new(Mutex::new(mgr))
    };

    let owner_lookup = Arc::new(MockOwnerLookup::new());
    owner_lookup.set_owner(alloc_id, owner.clone()).await;

    let pty_backend = Arc::new(MockPtyBackend::new());
    let attach_mgr = AttachManager::new(pty_backend, alloc_mgr, owner_lookup);

    // Attempt attach as wrong user
    let result = attach_mgr
        .attach(alloc_id, intruder.clone(), None, TerminalSize::default())
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AttachError::PermissionDenied {
            user,
            alloc_id: err_alloc_id,
            owner: err_owner,
        } => {
            assert_eq!(user, "eve");
            assert_eq!(err_alloc_id, alloc_id);
            assert_eq!(err_owner, "alice");
        }
        other => panic!("expected PermissionDenied, got: {other:?}"),
    }

    // No session was created
    assert_eq!(attach_mgr.active_session_count().await, 0);
}

// ===============================================================
// Test 13: Telemetry collector multi-mode switching
// ===============================================================

#[tokio::test]
async fn telemetry_collector_multi_mode() {
    use lattice_node_agent::telemetry::{TelemetryCollector, TelemetryMode, TelemetrySample};

    fn make_sample(gpu_util: f64, cpu_util: f64) -> TelemetrySample {
        TelemetrySample {
            timestamp: chrono::Utc::now(),
            gpu_utilization: vec![gpu_util],
            gpu_memory_used_gb: vec![20.0],
            cpu_utilization: cpu_util,
            memory_used_gb: 128.0,
            network_rx_gbps: 10.0,
            network_tx_gbps: 5.0,
            storage_read_mbps: 100.0,
            storage_write_mbps: 50.0,
        }
    }

    let mut collector = TelemetryCollector::new(TelemetryMode::Production);
    assert_eq!(collector.mode(), TelemetryMode::Production);
    assert_eq!(TelemetryMode::Production.interval_secs(), 30);

    // Record samples in Production mode.
    collector.record(make_sample(0.7, 0.4));
    collector.record(make_sample(0.8, 0.5));
    assert_eq!(collector.sample_count(), 2);

    // Switch to Debug mode.
    collector.set_mode(TelemetryMode::Debug);
    assert_eq!(collector.mode(), TelemetryMode::Debug);
    assert_eq!(TelemetryMode::Debug.interval_secs(), 1);

    // Samples from previous mode are preserved in the buffer.
    assert_eq!(collector.sample_count(), 2);

    // Record more samples in Debug mode.
    collector.record(make_sample(0.9, 0.6));
    assert_eq!(collector.sample_count(), 3);

    // Switch to Audit mode.
    collector.set_mode(TelemetryMode::Audit);
    assert_eq!(collector.mode(), TelemetryMode::Audit);
    assert_eq!(TelemetryMode::Audit.interval_secs(), 10);

    // Record one more sample in Audit mode.
    collector.record(make_sample(0.5, 0.3));
    assert_eq!(collector.sample_count(), 4);
}

// ===============================================================
// Test 14: Telemetry collector aggregation
// ===============================================================

#[tokio::test]
async fn telemetry_collector_aggregation() {
    use lattice_node_agent::telemetry::{TelemetryCollector, TelemetryMode, TelemetrySample};

    let mut collector = TelemetryCollector::new(TelemetryMode::Debug);

    // Empty aggregation returns None.
    assert!(collector.aggregate().is_none());

    let samples = vec![
        TelemetrySample {
            timestamp: chrono::Utc::now(),
            gpu_utilization: vec![0.4, 0.6],
            gpu_memory_used_gb: vec![10.0, 15.0],
            cpu_utilization: 0.3,
            memory_used_gb: 100.0,
            network_rx_gbps: 8.0,
            network_tx_gbps: 4.0,
            storage_read_mbps: 50.0,
            storage_write_mbps: 25.0,
        },
        TelemetrySample {
            timestamp: chrono::Utc::now(),
            gpu_utilization: vec![0.8, 0.9],
            gpu_memory_used_gb: vec![20.0, 25.0],
            cpu_utilization: 0.7,
            memory_used_gb: 200.0,
            network_rx_gbps: 12.0,
            network_tx_gbps: 6.0,
            storage_read_mbps: 150.0,
            storage_write_mbps: 75.0,
        },
        TelemetrySample {
            timestamp: chrono::Utc::now(),
            gpu_utilization: vec![0.6, 0.6],
            gpu_memory_used_gb: vec![15.0, 15.0],
            cpu_utilization: 0.5,
            memory_used_gb: 150.0,
            network_rx_gbps: 10.0,
            network_tx_gbps: 5.0,
            storage_read_mbps: 100.0,
            storage_write_mbps: 50.0,
        },
    ];

    for s in samples {
        collector.record(s);
    }

    let agg = collector.aggregate().unwrap();
    assert_eq!(agg.sample_count, 3);

    // Sample 1 avg GPU util: (0.4+0.6)/2 = 0.5
    // Sample 2 avg GPU util: (0.8+0.9)/2 = 0.85
    // Sample 3 avg GPU util: (0.6+0.6)/2 = 0.6
    // Overall avg: (0.5+0.85+0.6)/3 = 0.65
    assert!((agg.avg_gpu_utilization - 0.65).abs() < 0.001);

    // Max GPU util across samples: 0.85
    assert!((agg.max_gpu_utilization - 0.85).abs() < 0.001);

    // Avg CPU: (0.3+0.7+0.5)/3 = 0.5
    assert!((agg.avg_cpu_utilization - 0.5).abs() < 0.001);

    // Avg memory: (100+200+150)/3 = 150.0
    assert!((agg.avg_memory_used_gb - 150.0).abs() < 0.001);

    // Avg rx: (8+12+10)/3 = 10.0
    assert!((agg.avg_network_rx_gbps - 10.0).abs() < 0.001);

    // Avg tx: (4+6+5)/3 = 5.0
    assert!((agg.avg_network_tx_gbps - 5.0).abs() < 0.001);
}

// ===============================================================
// Test 15: Stub memory discovery -- default DRAM domain
// ===============================================================

#[tokio::test]
async fn stub_memory_discovery_default() {
    use lattice_common::types::MemoryDomainType;
    use lattice_node_agent::telemetry::memory_discovery::{
        MemoryDiscoveryProvider, StubMemoryDiscovery,
    };

    let stub = StubMemoryDiscovery::default();
    let topo = stub.discover().await.unwrap();

    assert_eq!(topo.domains.len(), 1);
    assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Dram);
    assert_eq!(topo.domains[0].id, 0);
    assert_eq!(topo.domains[0].numa_node, Some(0));
    assert_eq!(topo.domains[0].attached_cpus, vec![0, 1, 2, 3]);
    assert!(topo.domains[0].attached_gpus.is_empty());
    // Default is 512 GB
    assert_eq!(topo.total_capacity_bytes, 512 * 1024 * 1024 * 1024);
    assert_eq!(topo.domains[0].capacity_bytes, 512 * 1024 * 1024 * 1024);
    assert!(topo.interconnects.is_empty());
}

// ===============================================================
// Test 16: Stub memory discovery -- unified domain type
// ===============================================================

#[tokio::test]
async fn stub_memory_discovery_unified() {
    use lattice_common::types::MemoryDomainType;
    use lattice_node_agent::telemetry::memory_discovery::{
        MemoryDiscoveryProvider, StubMemoryDiscovery,
    };

    let cap = 256 * 1024 * 1024 * 1024u64;
    let stub = StubMemoryDiscovery::new(cap).with_type(MemoryDomainType::Unified);
    let topo = stub.discover().await.unwrap();

    assert_eq!(topo.domains.len(), 1);
    assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Unified);
    assert_eq!(topo.total_capacity_bytes, cap);
    assert_eq!(topo.domains[0].capacity_bytes, cap);
    assert_eq!(topo.domains[0].numa_node, Some(0));
}

// ===============================================================
// Test 17: Log buffer write/read cycle with wrap-around
// ===============================================================

#[tokio::test]
async fn log_buffer_write_read_cycle() {
    let mut buf = LogRingBuffer::with_capacity(16);

    // Write some data within capacity.
    buf.write(b"AAAA");
    buf.write(b"BBBB");
    assert_eq!(buf.len(), 8);
    assert_eq!(buf.read_all(), b"AAAABBBB");

    // Fill the buffer exactly.
    buf.write(b"CCCCCCCC");
    assert_eq!(buf.len(), 16);
    assert_eq!(buf.read_all(), b"AAAABBBBCCCCCCCC");

    // Write past capacity -- wraps around, overwriting oldest data.
    buf.write(b"1234");
    assert_eq!(buf.len(), 16);
    let data = buf.read_all();
    // AAAABBBB + CCCCCCCC = 16 bytes, then 1234 overwrites first 4 bytes.
    // write_pos = 20 % 16 = 4, so oldest starts at 4:
    // buf[4..16] + buf[0..4] = "BBBBCCCCCCCC" + "1234"
    assert_eq!(data, b"BBBBCCCCCCCC1234");

    // Write even more to wrap again.
    buf.write(b"XYZW");
    let data2 = buf.read_all();
    // write_pos = 24 % 16 = 8, buf[8..16] + buf[0..8]
    assert_eq!(data2, b"CCCCCCCC1234XYZW");
}

// ===============================================================
// Test 18: Log buffer S3 flush integration
// ===============================================================

#[tokio::test]
async fn log_buffer_s3_flush_integration() {
    let mut buf = LogRingBuffer::with_capacity(1024);
    buf.write(b"line 1: training epoch 1\n");
    buf.write(b"line 2: training epoch 2\n");
    buf.write(b"line 3: evaluation complete\n");

    let (s3, uploads) = MockS3::new();
    buf.flush_to_s3(&s3, "test-bucket", "logs/alloc-42.log")
        .await
        .unwrap();

    let uploads = uploads.lock().await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].0, "test-bucket");
    assert_eq!(uploads[0].1, "logs/alloc-42.log");

    let content = String::from_utf8(uploads[0].2.clone()).unwrap();
    assert!(content.contains("line 1: training epoch 1"));
    assert!(content.contains("line 2: training epoch 2"));
    assert!(content.contains("line 3: evaluation complete"));

    // Verify the data is in order: line 1 before line 2 before line 3.
    let pos1 = content.find("line 1").unwrap();
    let pos2 = content.find("line 2").unwrap();
    let pos3 = content.find("line 3").unwrap();
    assert!(pos1 < pos2);
    assert!(pos2 < pos3);
}

// ===============================================================
// Test 19: Mock data stage executor records calls
// ===============================================================

#[tokio::test]
async fn mock_data_stage_records_calls() {
    use lattice_common::types::{DataAccess, DataMount, StorageTier};
    use lattice_node_agent::data_stage::{DataStageExecutor, MockDataStageExecutor};

    let mock = MockDataStageExecutor::new();
    let alloc_id = Uuid::new_v4();
    let mounts = vec![
        DataMount {
            source: "s3://data/input".to_string(),
            target: "/mnt/input".to_string(),
            access: DataAccess::ReadOnly,
            tier_hint: Some(StorageTier::Hot),
        },
        DataMount {
            source: "nfs://nas/output".to_string(),
            target: "/mnt/output".to_string(),
            access: DataAccess::ReadWrite,
            tier_hint: Some(StorageTier::Hot),
        },
    ];

    let result = mock
        .stage_mounts(alloc_id, &mounts, Some("/scratch"))
        .await
        .unwrap();
    assert_eq!(result.mounts_staged, 2);
    assert_eq!(result.total_processed, 2);

    // Verify staging calls were recorded.
    let staged = mock.staged_calls();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].0, alloc_id);
    assert_eq!(staged[0].1.len(), 2);
    assert_eq!(staged[0].1[0].source, "s3://data/input");
    assert_eq!(staged[0].1[1].target, "/mnt/output");

    // Now cleanup.
    mock.cleanup_mounts(alloc_id, &mounts).await.unwrap();
    let cleaned = mock.cleaned_calls();
    assert_eq!(cleaned.len(), 1);
    assert_eq!(cleaned[0].0, alloc_id);
    assert_eq!(cleaned[0].1.len(), 2);
}

// ===============================================================
// Test 20: Mock data stage executor error injection
// ===============================================================

#[tokio::test]
async fn mock_data_stage_error_injection() {
    use lattice_common::types::{DataAccess, DataMount, StorageTier};
    use lattice_node_agent::data_stage::{DataStageExecutor, MockDataStageExecutor};

    let mock = MockDataStageExecutor::with_error("disk full");
    let alloc_id = Uuid::new_v4();
    let mounts = vec![DataMount {
        source: "s3://data/large-dataset".to_string(),
        target: "/mnt/data".to_string(),
        access: DataAccess::ReadOnly,
        tier_hint: Some(StorageTier::Hot),
    }];

    let result = mock.stage_mounts(alloc_id, &mounts, None).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("disk full"),
        "error should contain injected message, got: {err_msg}"
    );

    // Staging calls should not have been recorded.
    assert!(mock.staged_calls().is_empty());

    // Cleanup should still succeed (error only affects staging).
    mock.cleanup_mounts(alloc_id, &mounts).await.unwrap();
    assert_eq!(mock.cleaned_calls().len(), 1);
}

// ===============================================================
// Test 21: ProcSysCollector lifecycle (attach/collect/detach)
// ===============================================================

#[tokio::test]
async fn proc_collector_lifecycle() {
    use lattice_node_agent::telemetry::ebpf_stubs::{CollectorState, EbpfCollector};
    use lattice_node_agent::telemetry::proc_collector::ProcSysCollector;

    let mut collector = ProcSysCollector::new();

    // Starts detached.
    assert_eq!(collector.state(), CollectorState::Detached);

    // Cannot read events while detached.
    let err = collector.read_events().await;
    assert!(err.is_err());

    // Attach.
    collector.attach().await.unwrap();
    assert_eq!(collector.state(), CollectorState::Attached);

    // Double attach fails.
    assert!(collector.attach().await.is_err());

    // Collect a snapshot (returns system metrics; on macOS returns defaults).
    let snapshot = collector.collect().await;
    // On any platform, the snapshot struct should exist with sane defaults.
    assert!(snapshot.cpu.idle_percent >= 0.0);
    assert!(snapshot.cpu.total_percent >= 0.0);

    // Read events while attached returns a JSON-encoded event.
    let events = collector.read_events().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "system_snapshot");
    assert!(!events[0].payload.is_empty());

    // Detach.
    collector.detach().await.unwrap();
    assert_eq!(collector.state(), CollectorState::Detached);

    // Double detach fails.
    assert!(collector.detach().await.is_err());

    // Cannot read events after detach.
    assert!(collector.read_events().await.is_err());
}
