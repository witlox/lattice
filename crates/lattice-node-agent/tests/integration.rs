//! Integration tests for lattice-node-agent.
//!
//! These tests exercise multi-component interactions across module
//! boundaries: prologue + runtime + cache, epilogue + runtime + medical
//! wipe, full runtime lifecycle, agent heartbeat + health, signal
//! delivery through all modes, and image cache eviction.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use lattice_common::types::NodeCapabilities;
use lattice_node_agent::agent::{AgentCommand, NodeAgent};
use lattice_node_agent::checkpoint_handler::{CheckpointHandler, CheckpointMode};
use lattice_node_agent::conformance::ConformanceComponents;
use lattice_node_agent::data_stage::NoopDataStageExecutor;
use lattice_node_agent::epilogue::{
    EpilogueConfig, EpiloguePipeline, MedicalWiper, NoopEpilogueReporter, NoopMedicalWiper,
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

/// Mock medical wiper that records wipe calls.
struct RecordingMedicalWiper {
    wiped: Arc<Mutex<AllocIdLog>>,
}

impl RecordingMedicalWiper {
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
impl MedicalWiper for RecordingMedicalWiper {
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
    };

    // First prologue run: cache miss, runtime.prepare called, cache updated.
    let result_1 = pipeline
        .execute(
            alloc_id_1,
            &config_1,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
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
    };

    let result_2 = pipeline
        .execute(
            alloc_id_2,
            &config_2,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
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
// Test 2: Epilogue + Runtime + Medical wipe
// ===============================================================

#[tokio::test]
async fn epilogue_runtime_medical_wipe() {
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
    };
    runtime.prepare(&prep_config).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let (wiper, wiped_ids) = RecordingMedicalWiper::new();

    // Epilogue with medical_wipe = true.
    let pipeline = EpiloguePipeline::new(EpilogueConfig {
        log_bucket: "medical-logs".to_string(),
        medical_wipe: true,
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
            &wiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    // Medical wipe was triggered.
    assert!(result.medical_wiped, "medical wipe should have occurred");
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
// Test 7: Epilogue with log flush + runtime cleanup + no medical wipe
// ===============================================================

#[tokio::test]
async fn epilogue_log_flush_and_cleanup_no_medical() {
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
            &NoopMedicalWiper,
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

    // No medical wipe (default config has medical_wipe=false).
    assert!(!result.medical_wiped);

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
            })
            .await
            .unwrap();

        cmd_tx
            .send(AgentCommand::StartAllocation {
                id: alloc_2,
                entrypoint: "eval.py".to_string(),
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
    };

    let prologue = ProloguePipeline::default();
    let prologue_result = prologue
        .execute(
            alloc_id,
            &prep_config,
            &runtime,
            &mut cache,
            &NoopDataStageExecutor,
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
            &NoopMedicalWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();

    assert!(epilogue_result.logs_flushed);
    assert!(epilogue_result.cleaned_up);
    assert!(!epilogue_result.medical_wiped);
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
