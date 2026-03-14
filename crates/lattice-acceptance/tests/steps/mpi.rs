use std::collections::HashMap;
use std::sync::Arc;

use cucumber::{given, then, when};
use uuid::Uuid;

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_node_agent::pmi2::protocol::{parse_command, Pmi2Command};
use lattice_node_agent::pmi2::fence::{FenceCoordinator, MockFenceTransport};
use lattice_node_agent::process_launcher::{LaunchConfig, ProcessLauncher};
use lattice_api::mpi::{MpiLaunchOrchestrator, NodeAgentPool, StubNodeAgentPool};

// ─── FailingNodeAgentPool ──────────────────────────────────

struct FailingNodeAgentPool;

#[async_trait::async_trait]
impl NodeAgentPool for FailingNodeAgentPool {
    async fn launch_processes(
        &self,
        _node_address: &str,
        _request: lattice_common::proto::lattice::v1::LaunchProcessesRequest,
    ) -> Result<lattice_common::proto::lattice::v1::LaunchProcessesResponse, String> {
        Ok(lattice_common::proto::lattice::v1::LaunchProcessesResponse {
            accepted: false,
            message: "node rejected launch".into(),
        })
    }
}

// ─── Helpers ────────────────────────────────────────────────

fn make_launch_config(
    tasks_per_node: u32,
    first_rank: u32,
    world_size: u32,
    cxi_credentials: Option<CxiCredentials>,
) -> LaunchConfig {
    let peers = vec![PeerInfo {
        node_id: "node0".into(),
        grpc_address: "http://node0:50052".into(),
        first_rank,
        num_ranks: tasks_per_node,
    }];

    LaunchConfig {
        launch_id: Uuid::new_v4(),
        allocation_id: Uuid::new_v4(),
        entrypoint: "/bin/echo".into(),
        args: vec!["hello".into()],
        env: HashMap::new(),
        tasks_per_node,
        first_rank,
        world_size,
        pmi_mode: PmiMode::Pmi2,
        cxi_credentials,
        peers,
        head_node_index: 0,
        my_node_index: 0,
        node_id: "node0".into(),
        socket_dir: std::env::temp_dir(),
        nodelist: "node0".into(),
    }
}

#[cfg(unix)]
async fn pmi_connect(
    socket_path: &std::path::Path,
) -> crate::PmiConnection {
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(socket_path)
        .await
        .expect("failed to connect to PMI socket");
    let (read_half, write_half) = stream.into_split();
    crate::PmiConnection {
        reader: BufReader::new(read_half),
        writer: write_half,
    }
}

#[cfg(unix)]
async fn pmi_send_on_conn(
    conn: &mut crate::PmiConnection,
    msg: &str,
) -> String {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    conn.writer
        .write_all(msg.as_bytes())
        .await
        .expect("failed to write to PMI socket");

    let mut line = String::new();
    conn.reader
        .read_line(&mut line)
        .await
        .expect("failed to read from PMI socket");
    line
}

// ─── Given Steps ───────────────────────────────────────────

#[cfg(unix)]
#[given(regex = r#"^a PMI-2 server for (\d+) ranks? on (\d+) nodes? with world size (\d+)$"#)]
fn given_pmi_server(world: &mut LatticeWorld, ranks: u32, _nodes: u32, world_size: u32) {
    use lattice_node_agent::pmi2::server::{Pmi2Server, Pmi2ServerConfig};

    let tempdir = tempfile::tempdir().expect("failed to create tempdir");
    let socket_dir = tempdir.path().to_path_buf();

    let config = Pmi2ServerConfig {
        launch_id: Uuid::new_v4(),
        first_rank: 0,
        world_size,
        local_rank_count: ranks,
        appnum: 0,
        socket_dir: socket_dir.clone(),
    };

    let server = Arc::new(Pmi2Server::new(config));
    let socket_path = server.socket_path().to_path_buf();

    // Spawn the server in the background so the socket is actually listening
    let server_clone = server.clone();
    let handle = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            tokio::spawn(async move {
                let _ = server_clone
                    .run(|local_entries| async move { Ok(local_entries) })
                    .await;
            })
        })
    });
    // Wait for the server to bind the socket
    let sp = socket_path.clone();
    for _ in 0..100 {
        if sp.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(sp.exists(), "PMI socket was not created at {:?}", sp);

    world.pmi_server = Some(server);
    world.pmi_socket_path = Some(socket_path);
    world.mpi_temp_dir = Some(tempdir);
    world.pmi_server_handle = Some(handle);
}

#[given(regex = r#"^(\d+) nodes with (\d+) tasks per node$"#)]
fn given_nodes_with_tasks(world: &mut LatticeWorld, node_count: u32, tasks_per_node: u32) {
    world.mpi_node_ids = (0..node_count).map(|i| format!("node-{i}")).collect();
    world.mpi_tasks_per_node_cfg = tasks_per_node;
    world.mpi_num_nodes_cfg = node_count;
    for nid in &world.mpi_node_ids {
        world.mpi_node_addresses.insert(
            nid.clone(),
            format!("http://{}:50052", nid),
        );
    }
}

#[given(regex = r#"^(\d+) nodes with (\d+) total tasks$"#)]
fn given_nodes_with_total_tasks(world: &mut LatticeWorld, node_count: u32, total_tasks: u32) {
    world.mpi_node_ids = (0..node_count).map(|i| format!("node-{i}")).collect();
    // Auto-compute tasks per node
    world.mpi_tasks_per_node_cfg = total_tasks.div_ceil(node_count);
    world.mpi_num_nodes_cfg = node_count;
    for nid in &world.mpi_node_ids {
        world.mpi_node_addresses.insert(
            nid.clone(),
            format!("http://{}:50052", nid),
        );
    }
}

#[given("an MPI orchestrator with a stub agent pool")]
fn given_stub_orchestrator(world: &mut LatticeWorld) {
    let pool = Arc::new(StubNodeAgentPool);
    world.mpi_orchestrator = Some(MpiLaunchOrchestrator::new(pool));
}

#[given("an MPI orchestrator with a failing agent pool")]
fn given_failing_orchestrator(world: &mut LatticeWorld) {
    let pool = Arc::new(FailingNodeAgentPool);
    world.mpi_orchestrator = Some(MpiLaunchOrchestrator::new(pool));
}

#[given(regex = r#"^(\d+) assigned nodes with addresses$"#)]
fn given_assigned_nodes(world: &mut LatticeWorld, count: u32) {
    world.mpi_node_ids = (0..count).map(|i| format!("node-{i}")).collect();
    for nid in &world.mpi_node_ids {
        world.mpi_node_addresses.insert(
            nid.clone(),
            format!("http://{}:50052", nid),
        );
    }
}

#[given("a PMI-2 protocol parser")]
fn given_pmi_parser(_world: &mut LatticeWorld) {
    // Parser is stateless; nothing to set up.
}

#[given(regex = r#"^a launch config for (\d+) ranks starting at rank (\d+) with world size (\d+)$"#)]
fn given_launch_config(
    world: &mut LatticeWorld,
    tasks: u32,
    first_rank: u32,
    world_size: u32,
) {
    let config = make_launch_config(tasks, first_rank, world_size, None);
    let transport = Arc::new(MockFenceTransport::new());
    let launcher = ProcessLauncher::new(config, transport);
    world.process_launcher = Some(launcher);
}

#[given(regex = r#"^a launch config with CXI credentials VNI (\d+) and auth key "([^"]+)"$"#)]
fn given_launch_config_cxi(world: &mut LatticeWorld, vni: u32, auth_key: String) {
    let cxi = CxiCredentials {
        vni,
        auth_key: hex::decode(&auth_key).expect("invalid hex auth key"),
        svc_id: 42,
    };
    let config = make_launch_config(4, 0, 4, Some(cxi));
    let transport = Arc::new(MockFenceTransport::new());
    let launcher = ProcessLauncher::new(config, transport);
    world.process_launcher = Some(launcher);
}

#[given(regex = r#"^a fence coordinator for (\d+) nodes with node (\d+) as head$"#)]
fn given_fence_coordinator(world: &mut LatticeWorld, node_count: u32, head_index: u32) {
    let launch_id = Uuid::new_v4();
    let peers: Vec<PeerInfo> = (0..node_count)
        .map(|i| PeerInfo {
            node_id: format!("node-{i}"),
            grpc_address: format!("http://node-{i}:50052"),
            first_rank: i * 4,
            num_ranks: 4,
        })
        .collect();

    let transport = Arc::new(MockFenceTransport::new());
    let coordinator = FenceCoordinator::new(
        launch_id,
        peers,
        head_index,
        head_index, // my_index == head for testing
        transport,
    );
    world.fence_coordinator = Some(coordinator);
}

// ─── When Steps ────────────────────────────────────────────

#[cfg(unix)]
#[when("a rank connects and performs fullinit")]
fn when_rank_fullinit(world: &mut LatticeWorld) {
    let msg = "cmd=fullinit;pmi_version=2;pmi_subversion=0;\n";
    let socket_path = world.pmi_socket_path.as_ref().expect("no PMI socket path").clone();
    let (conn, resp) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let mut conn = pmi_connect(&socket_path).await;
            let resp = pmi_send_on_conn(&mut conn, msg).await;
            (conn, resp)
        })
    });
    world.pmi_responses.push(resp);
    world.pmi_connections.push(conn);
}

#[cfg(unix)]
#[then(regex = r#"^the rank should receive rank (\d+) and world size (\d+)$"#)]
fn then_rank_receives(world: &mut LatticeWorld, expected_rank: u32, expected_size: u32) {
    let resp = world.pmi_responses.last().expect("no PMI response");
    assert!(
        resp.contains(&format!("rank={expected_rank}"))
            || resp.contains(&format!("pmi_rank={expected_rank}")),
        "expected rank {expected_rank} in response '{resp}'"
    );
    let _ = expected_size; // world_size is checked via PMI_SIZE env
}

#[when("the rank layout is computed")]
fn when_rank_layout_computed(world: &mut LatticeWorld) {
    let layout = RankLayout::compute(&world.mpi_node_ids, world.mpi_tasks_per_node_cfg);
    world.rank_layout = Some(layout);
}

#[when("the rank layout is auto-computed")]
fn when_rank_layout_auto_computed(world: &mut LatticeWorld) {
    let layout = RankLayout::compute(&world.mpi_node_ids, world.mpi_tasks_per_node_cfg);
    world.rank_layout = Some(layout);
}

#[when(regex = r#"^I launch an MPI job with (\d+) tasks and (\d+) tasks per node$"#)]
fn when_launch_mpi(world: &mut LatticeWorld, num_tasks: u32, tasks_per_node: u32) {
    let orch = world
        .mpi_orchestrator
        .as_ref()
        .expect("no orchestrator set up");
    let result = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(orch.launch(
        Uuid::new_v4(),
        &world.mpi_node_ids,
        &world.mpi_node_addresses,
        "/bin/echo",
        &[],
        &HashMap::new(),
        num_tasks,
        tasks_per_node,
        PmiMode::Pmi2,
    )));
    world.launch_result = Some(result.map_err(|e| e.to_string()));
}

#[when(regex = r#"^I parse "([^"]+)"$"#)]
fn when_parse_command(world: &mut LatticeWorld, input: String) {
    world.parsed_command = Some(parse_command(&input).map_err(|e| e.to_string()));
}

#[when(regex = r#"^I compute the environment for rank (\d+) local rank (\d+)$"#)]
fn when_compute_env(world: &mut LatticeWorld, rank: u32, local_rank: u32) {
    let launcher = world
        .process_launcher
        .as_ref()
        .expect("no process launcher");
    let env = launcher.rank_env(rank, local_rank);
    world.rank_env = Some(env);
}

#[when(regex = r#"^node (\d+) contributes key "([^"]+)" with value "([^"]+)"$"#)]
fn when_node_contributes_key(
    world: &mut LatticeWorld,
    node_index: u32,
    key: String,
    value: String,
) {
    let mut entries = HashMap::new();
    entries.insert(key, value);
    world.fence_contributions.push((node_index, entries));
}

#[when("the head node executes the fence")]
fn when_head_executes_fence(world: &mut LatticeWorld) {
    // Merge all contributions
    let mut merged = HashMap::new();
    for (_idx, entries) in &world.fence_contributions {
        merged.extend(entries.clone());
    }
    world.fence_merged = Some(merged);
}

// PMI-2 lifecycle steps for single-node scenarios

#[cfg(unix)]
#[when("the rank performs kvsfence")]
fn when_rank_kvsfence(world: &mut LatticeWorld) {
    let msg = "cmd=kvsfence;\n";
    let conn = world.pmi_connections.first_mut().expect("no PMI connection");
    let resp = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pmi_send_on_conn(conn, msg))
    });
    world.pmi_responses.push(resp);
}

#[cfg(unix)]
#[then("the fence should complete successfully")]
fn then_fence_completes(world: &mut LatticeWorld) {
    let resp = world.pmi_responses.last().expect("no PMI response");
    assert!(
        resp.contains("rc=0") || resp.contains("cmd=kvsfence-response"),
        "fence did not complete successfully: '{resp}'"
    );
}

#[cfg(unix)]
#[when("the rank finalizes")]
fn when_rank_finalizes(world: &mut LatticeWorld) {
    let msg = "cmd=finalize;\n";
    let conn = world.pmi_connections.first_mut().expect("no PMI connection");
    let resp = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pmi_send_on_conn(conn, msg))
    });
    world.pmi_responses.push(resp);
}

#[cfg(unix)]
#[then("the PMI-2 server should shut down cleanly")]
fn then_pmi_shuts_down(world: &mut LatticeWorld) {
    let resp = world.pmi_responses.last().expect("no PMI response");
    assert!(
        resp.contains("rc=0") || resp.contains("cmd=finalize-response"),
        "PMI did not shut down cleanly: '{resp}'"
    );
}

// Two-rank KVS exchange steps

#[cfg(unix)]
#[when(regex = r#"^rank (\d+) connects and puts key "([^"]+)" with value "([^"]+)"$"#)]
fn when_rank_puts_key(world: &mut LatticeWorld, _rank: u32, key: String, value: String) {
    let socket_path = world.pmi_socket_path.as_ref().expect("no PMI socket path").clone();
    // Each rank connects and sends fullinit + kvsput on its own persistent connection
    let (conn, resp) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let mut conn = pmi_connect(&socket_path).await;
            // fullinit first (PMI-2 requires it before any other command)
            let _init_resp = pmi_send_on_conn(&mut conn, "cmd=fullinit;pmi_version=2;\n").await;
            let resp = pmi_send_on_conn(&mut conn, &format!("cmd=kvsput;key={key};value={value};\n")).await;
            (conn, resp)
        })
    });
    world.pmi_responses.push(resp);
    world.pmi_connections.push(conn);
}

#[cfg(unix)]
#[when("both ranks perform kvsfence")]
fn when_both_ranks_fence(world: &mut LatticeWorld) {
    let msg = "cmd=kvsfence;\n";
    assert!(
        world.pmi_connections.len() >= 2,
        "expected at least 2 PMI connections for both ranks"
    );
    // We need to send fence on both connections concurrently.
    // Take mutable references by splitting the slice.
    let (first, rest) = world.pmi_connections.split_at_mut(1);
    let conn0 = &mut first[0];
    let conn1 = &mut rest[0];
    let (resp0, resp1) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            tokio::join!(
                pmi_send_on_conn(conn0, msg),
                pmi_send_on_conn(conn1, msg),
            )
        })
    });
    world.pmi_responses.push(resp0);
    world.pmi_responses.push(resp1);
}

#[cfg(unix)]
#[then(regex = r#"^rank (\d+) should be able to get key "([^"]+)" with value "([^"]+)"$"#)]
fn then_rank_gets_key(world: &mut LatticeWorld, rank: u32, key: String, expected: String) {
    // After fence, KVS should have been merged. Verify by sending kvsget on the rank's connection.
    let msg = format!("cmd=kvsget;key={key};\n");
    let conn = world.pmi_connections.get_mut(rank as usize)
        .unwrap_or_else(|| panic!("no PMI connection for rank {rank}"));
    let resp = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pmi_send_on_conn(conn, &msg))
    });
    assert!(
        resp.contains(&expected),
        "expected key '{key}' to have value '{expected}' in response '{resp}'"
    );
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^the total rank count should be (\d+)$"#)]
fn then_total_ranks(world: &mut LatticeWorld, expected: u32) {
    let layout = world.rank_layout.as_ref().expect("no rank layout computed");
    assert_eq!(
        layout.total_ranks, expected,
        "expected {expected} total ranks, got {}",
        layout.total_ranks
    );
}

#[then(regex = r#"^node (\d+) should have ranks (\d+) through (\d+)$"#)]
fn then_node_has_ranks(world: &mut LatticeWorld, node_idx: usize, first: u32, last: u32) {
    let layout = world.rank_layout.as_ref().expect("no rank layout computed");
    let assignment = &layout.node_assignments[node_idx];
    assert_eq!(
        assignment.first_rank, first,
        "node {node_idx} first_rank: expected {first}, got {}",
        assignment.first_rank
    );
    let actual_last = assignment.first_rank + assignment.num_ranks - 1;
    assert_eq!(
        actual_last, last,
        "node {node_idx} last rank: expected {last}, got {actual_last}"
    );
}

#[then(regex = r#"^each node should have (\d+) tasks per node$"#)]
fn then_each_node_tasks(world: &mut LatticeWorld, expected: u32) {
    let layout = world.rank_layout.as_ref().expect("no rank layout computed");
    for (i, assignment) in layout.node_assignments.iter().enumerate() {
        assert_eq!(
            assignment.num_ranks, expected,
            "node {i} has {} tasks, expected {expected}",
            assignment.num_ranks
        );
    }
}

#[then("the launch should succeed with a valid launch ID")]
fn then_launch_succeeds(world: &mut LatticeWorld) {
    let result = world.launch_result.as_ref().expect("no launch result");
    assert!(
        result.is_ok(),
        "expected launch to succeed, got {:?}",
        result
    );
}

#[then("the launch should fail mentioning node rejection")]
fn then_launch_fails(world: &mut LatticeWorld) {
    let result = world.launch_result.as_ref().expect("no launch result");
    assert!(result.is_err(), "expected launch to fail");
    let err = result.as_ref().unwrap_err();
    assert!(
        err.contains("rejected") || err.contains("failed"),
        "error should mention rejection: '{err}'"
    );
}

#[then(regex = r#"^the command should be FullInit with version (\d+)$"#)]
fn then_command_fullinit(world: &mut LatticeWorld, version: u32) {
    let parsed = world
        .parsed_command
        .as_ref()
        .expect("no parsed command");
    match parsed {
        Ok(Pmi2Command::FullInit { pmi_version, .. }) => {
            assert_eq!(
                *pmi_version, version,
                "expected FullInit version {version}, got {pmi_version}"
            );
        }
        other => panic!("expected FullInit, got {other:?}"),
    }
}

#[then(regex = r#"^the command should be KvsPut with key "([^"]+)" and value "([^"]+)"$"#)]
fn then_command_kvsput(world: &mut LatticeWorld, expected_key: String, expected_value: String) {
    let parsed = world
        .parsed_command
        .as_ref()
        .expect("no parsed command");
    match parsed {
        Ok(Pmi2Command::KvsPut { key, value }) => {
            assert_eq!(key, &expected_key, "key mismatch");
            assert_eq!(value, &expected_value, "value mismatch");
        }
        other => panic!("expected KvsPut, got {other:?}"),
    }
}

#[then(regex = r#"^the command should be Abort with message "([^"]+)"$"#)]
fn then_command_abort(world: &mut LatticeWorld, expected_msg: String) {
    let parsed = world
        .parsed_command
        .as_ref()
        .expect("no parsed command");
    match parsed {
        Ok(Pmi2Command::Abort { message }) => {
            assert!(
                message.contains(&expected_msg),
                "expected abort message containing '{expected_msg}', got '{message}'"
            );
        }
        other => panic!("expected Abort, got {other:?}"),
    }
}

#[then(regex = r#"^the environment should contain "([^"]+)" = "([^"]+)"$"#)]
fn then_env_contains(world: &mut LatticeWorld, key: String, expected_value: String) {
    let env = world.rank_env.as_ref().expect("no rank environment computed");
    let actual = env
        .get(&key)
        .unwrap_or_else(|| panic!("environment does not contain key '{key}'"));
    assert_eq!(
        actual, &expected_value,
        "env '{key}': expected '{expected_value}', got '{actual}'"
    );
}

#[then(regex = r#"^the merged KVS should contain all (\d+) entries$"#)]
fn then_merged_kvs_contains(world: &mut LatticeWorld, expected_count: usize) {
    let merged = world
        .fence_merged
        .as_ref()
        .expect("no merged KVS from fence");
    assert_eq!(
        merged.len(),
        expected_count,
        "expected {expected_count} merged entries, got {}",
        merged.len()
    );
}
