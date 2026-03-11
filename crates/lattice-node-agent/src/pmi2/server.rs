//! PMI-2 server: Unix domain socket server for MPI rank processes.
//!
//! Each launch gets a dedicated PMI-2 server that accepts one connection
//! per rank and handles the PMI-2 wire protocol.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{watch, Mutex};
use tracing::{debug, error, warn};

use super::kvs::LocalKvs;
use super::protocol::{format_response, parse_command, Pmi2Command, Pmi2Response};
use lattice_common::types::LaunchId;

/// Configuration for a PMI-2 server instance.
#[derive(Debug, Clone)]
pub struct Pmi2ServerConfig {
    pub launch_id: LaunchId,
    pub first_rank: u32,
    pub world_size: u32,
    pub local_rank_count: u32,
    pub appnum: u32,
    /// Directory for the Unix socket (default: /tmp).
    pub socket_dir: PathBuf,
}

/// Job-level attributes queryable via `job-getinfo`.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub attrs: HashMap<String, String>,
}

impl JobInfo {
    pub fn new(world_size: u32) -> Self {
        let mut attrs = HashMap::new();
        attrs.insert("universeSize".to_string(), world_size.to_string());
        attrs.insert("hasNameServ".to_string(), "0".to_string());
        attrs.insert("PMIversion".to_string(), "2".to_string());
        Self { attrs }
    }
}

/// A running PMI-2 server for one MPI launch on one node.
pub struct Pmi2Server {
    config: Pmi2ServerConfig,
    kvs: Arc<LocalKvs>,
    job_info: Arc<JobInfo>,
    socket_path: PathBuf,
    finalized_count: Arc<Mutex<u32>>,
    abort_tx: watch::Sender<bool>,
    abort_rx: watch::Receiver<bool>,
}

impl std::fmt::Debug for Pmi2Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pmi2Server")
            .field("config", &self.config)
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Pmi2Server {
    pub fn new(config: Pmi2ServerConfig) -> Self {
        // Use short hex prefix to keep path under SUN_LEN (104 on macOS).
        let short_id = &config.launch_id.as_simple().to_string()[..12];
        let socket_path = config.socket_dir.join(format!("pmi-{short_id}.sock"));
        let kvs = Arc::new(LocalKvs::new(config.local_rank_count));
        let job_info = Arc::new(JobInfo::new(config.world_size));
        let (abort_tx, abort_rx) = watch::channel(false);

        Self {
            config,
            kvs,
            job_info,
            socket_path,
            finalized_count: Arc::new(Mutex::new(0)),
            abort_tx,
            abort_rx,
        }
    }

    /// Path to the Unix domain socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Reference to the local KVS (used by fence coordinator).
    pub fn kvs(&self) -> &Arc<LocalKvs> {
        &self.kvs
    }

    /// Signal all rank handlers to abort.
    pub fn abort(&self) {
        let _ = self.abort_tx.send(true);
    }

    /// Start the server, accepting connections from local ranks.
    ///
    /// Returns when all ranks have finalized or the server is aborted.
    /// The `on_all_fenced` callback is invoked when all local ranks
    /// have entered the fence barrier (triggering cross-node exchange).
    pub async fn run<F, Fut>(&self, on_all_fenced: F) -> Result<(), String>
    where
        F: Fn(HashMap<String, String>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<HashMap<String, String>, String>> + Send,
    {
        // Clean up any stale socket
        let _ = tokio::fs::remove_file(&self.socket_path).await;

        #[cfg(unix)]
        {
            let listener = tokio::net::UnixListener::bind(&self.socket_path)
                .map_err(|e| format!("bind {}: {e}", self.socket_path.display()))?;

            debug!(
                socket = %self.socket_path.display(),
                ranks = self.config.local_rank_count,
                "PMI-2 server listening"
            );

            let on_all_fenced = Arc::new(on_all_fenced);
            let mut rank_offset = 0u32;
            let mut handles = Vec::new();

            loop {
                if rank_offset >= self.config.local_rank_count {
                    break;
                }

                let abort_rx = self.abort_rx.clone();
                let accept = tokio::select! {
                    result = listener.accept() => result,
                    _ = Self::wait_abort(abort_rx) => {
                        warn!("PMI-2 server aborted before all ranks connected");
                        break;
                    }
                };

                match accept {
                    Ok((stream, _)) => {
                        let global_rank = self.config.first_rank + rank_offset;
                        let kvs = self.kvs.clone();
                        let job_info = self.job_info.clone();
                        let world_size = self.config.world_size;
                        let appnum = self.config.appnum;
                        let launch_id = self.config.launch_id;
                        let finalized = self.finalized_count.clone();
                        let abort_rx = self.abort_rx.clone();
                        let on_all_fenced = on_all_fenced.clone();

                        let handle = tokio::spawn(async move {
                            if let Err(e) = handle_rank(
                                stream,
                                global_rank,
                                world_size,
                                appnum,
                                launch_id,
                                kvs,
                                job_info,
                                finalized,
                                abort_rx,
                                on_all_fenced,
                            )
                            .await
                            {
                                error!(rank = global_rank, error = %e, "rank handler failed");
                            }
                        });
                        handles.push(handle);
                        rank_offset += 1;
                    }
                    Err(e) => {
                        error!(error = %e, "accept failed");
                        break;
                    }
                }
            }

            // Wait for all rank handlers to finish
            for h in handles {
                let _ = h.await;
            }

            // Clean up socket
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        #[cfg(not(unix))]
        {
            let _ = on_all_fenced;
            return Err("PMI-2 Unix socket server requires Unix".to_string());
        }

        Ok(())
    }

    async fn wait_abort(mut rx: watch::Receiver<bool>) {
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn handle_rank<F, Fut>(
    stream: tokio::net::UnixStream,
    rank: u32,
    world_size: u32,
    appnum: u32,
    launch_id: LaunchId,
    kvs: Arc<LocalKvs>,
    job_info: Arc<JobInfo>,
    finalized: Arc<Mutex<u32>>,
    abort_rx: watch::Receiver<bool>,
    on_all_fenced: Arc<F>,
) -> Result<(), String>
where
    F: Fn(HashMap<String, String>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<HashMap<String, String>, String>> + Send,
{
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = Pmi2Server::wait_abort(abort_rx.clone()) => {
                debug!(rank, "rank handler aborted");
                return Ok(());
            }
        };

        let line = match line {
            Ok(Some(l)) => l,
            Ok(None) => {
                debug!(rank, "rank disconnected");
                return Ok(());
            }
            Err(e) => return Err(format!("read error rank {rank}: {e}")),
        };

        let cmd = parse_command(&line).map_err(|e| format!("rank {rank} parse error: {e}"))?;
        debug!(rank, ?cmd, "PMI-2 command");

        let response = match cmd {
            Pmi2Command::FullInit { .. } => Pmi2Response::FullInitResp {
                rank,
                size: world_size,
                appnum,
                pmi_version: 2,
                pmi_subversion: 0,
                spawner_jobid: format!("lattice-{launch_id}"),
                rc: 0,
            },
            Pmi2Command::JobGetInfo { key } => {
                let (value, found) = match job_info.attrs.get(&key) {
                    Some(v) => (v.clone(), true),
                    None => (String::new(), false),
                };
                Pmi2Response::JobGetInfoResp {
                    key,
                    value,
                    found,
                    rc: 0,
                }
            }
            Pmi2Command::KvsPut { key, value } => {
                kvs.put(key, value).await;
                Pmi2Response::KvsPutResp { rc: 0 }
            }
            Pmi2Command::KvsGet { key } => match kvs.get(&key).await {
                Some(value) => Pmi2Response::KvsGetResp {
                    key,
                    value,
                    found: true,
                    rc: 0,
                },
                None => Pmi2Response::KvsGetResp {
                    key,
                    value: String::new(),
                    found: false,
                    rc: 0,
                },
            },
            Pmi2Command::KvsFence => {
                let (all_local, gen) = kvs.enter_fence().await;
                if all_local {
                    // All local ranks reached fence — trigger cross-node exchange
                    let local_entries = kvs.drain_pending().await;
                    match on_all_fenced(local_entries).await {
                        Ok(merged) => {
                            kvs.complete_fence(merged).await;
                        }
                        Err(e) => {
                            error!(error = %e, "cross-node fence failed");
                            return Err(format!("fence failed: {e}"));
                        }
                    }
                } else {
                    // Wait for the fence generation to advance
                    kvs.wait_fence(gen).await;
                }
                Pmi2Response::KvsFenceResp { rc: 0 }
            }
            Pmi2Command::Finalize => {
                let resp = Pmi2Response::FinalizeResp { rc: 0 };
                let wire = format_response(&resp);
                writer
                    .write_all(wire.as_bytes())
                    .await
                    .map_err(|e| format!("write error rank {rank}: {e}"))?;
                let mut count = finalized.lock().await;
                *count += 1;
                debug!(rank, finalized = *count, "rank finalized");
                return Ok(());
            }
            Pmi2Command::Abort { message } => {
                warn!(rank, message, "rank aborted");
                return Err(format!("rank {rank} aborted: {message}"));
            }
        };

        let wire = format_response(&response);
        writer
            .write_all(wire.as_bytes())
            .await
            .map_err(|e| format!("write error rank {rank}: {e}"))?;
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    async fn send_recv(stream: &mut UnixStream, msg: &str) -> String {
        let (reader, mut writer) = stream.split();
        writer.write_all(msg.as_bytes()).await.unwrap();
        let mut lines = BufReader::new(reader);
        let mut buf = String::new();
        lines.read_line(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn single_rank_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 1,
            local_rank_count: 1,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Pmi2Server::new(config);
        let socket_path = server.socket_path().to_path_buf();

        let server_handle = tokio::spawn(async move {
            server
                .run(|local_entries| async move { Ok(local_entries) })
                .await
                .unwrap();
        });

        // Small delay for server to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&socket_path).await.unwrap();

        // fullinit
        let resp = send_recv(&mut stream, "cmd=fullinit;pmi_version=2;\n").await;
        assert!(resp.contains("rank=0"));
        assert!(resp.contains("size=1"));

        // kvsput
        let resp = send_recv(&mut stream, "cmd=kvsput;key=addr;value=tcp://1.2.3.4;\n").await;
        assert!(resp.contains("rc=0"));

        // kvsfence (single rank, trivial)
        let resp = send_recv(&mut stream, "cmd=kvsfence;\n").await;
        assert!(resp.contains("rc=0"));

        // kvsget
        let resp = send_recv(&mut stream, "cmd=kvsget;key=addr;\n").await;
        assert!(resp.contains("found=true"));
        assert!(resp.contains("tcp://1.2.3.4"));

        // finalize
        let resp = send_recv(&mut stream, "cmd=finalize;\n").await;
        assert!(resp.contains("rc=0"));

        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server timeout")
            .expect("server join");
    }

    #[tokio::test]
    async fn two_rank_fence() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 2,
            local_rank_count: 2,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Arc::new(Pmi2Server::new(config));
        let socket_path = server.socket_path().to_path_buf();

        let server_clone = server.clone();
        let server_handle = tokio::spawn(async move {
            server_clone
                .run(|local_entries| async move { Ok(local_entries) })
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect two "ranks"
        let mut r0 = UnixStream::connect(&socket_path).await.unwrap();
        let mut r1 = UnixStream::connect(&socket_path).await.unwrap();

        // Both do fullinit
        let resp0 = send_recv(&mut r0, "cmd=fullinit;\n").await;
        assert!(resp0.contains("rank=0"));
        let resp1 = send_recv(&mut r1, "cmd=fullinit;\n").await;
        assert!(resp1.contains("rank=1"));

        // Each puts a key
        send_recv(&mut r0, "cmd=kvsput;key=addr0;value=rank0data;\n").await;
        send_recv(&mut r1, "cmd=kvsput;key=addr1;value=rank1data;\n").await;

        // Both fence concurrently
        let (f0, f1) = tokio::join!(
            send_recv(&mut r0, "cmd=kvsfence;\n"),
            send_recv(&mut r1, "cmd=kvsfence;\n"),
        );
        assert!(f0.contains("rc=0"));
        assert!(f1.contains("rc=0"));

        // Both can get each other's keys
        let g0 = send_recv(&mut r0, "cmd=kvsget;key=addr1;\n").await;
        assert!(g0.contains("rank1data"));
        let g1 = send_recv(&mut r1, "cmd=kvsget;key=addr0;\n").await;
        assert!(g1.contains("rank0data"));

        // Finalize
        send_recv(&mut r0, "cmd=finalize;\n").await;
        send_recv(&mut r1, "cmd=finalize;\n").await;

        drop(r0);
        drop(r1);
        tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server timeout")
            .expect("server join");
    }

    #[tokio::test]
    async fn job_getinfo_returns_universe_size() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 64,
            local_rank_count: 1,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Pmi2Server::new(config);
        let socket_path = server.socket_path().to_path_buf();

        let server_handle = tokio::spawn(async move {
            server.run(|e| async move { Ok(e) }).await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = UnixStream::connect(&socket_path).await.unwrap();

        send_recv(&mut stream, "cmd=fullinit;\n").await;
        let resp = send_recv(&mut stream, "cmd=job-getinfo;key=universeSize;\n").await;
        assert!(resp.contains("value=64"));
        assert!(resp.contains("found=true"));

        send_recv(&mut stream, "cmd=finalize;\n").await;
        drop(stream);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn abort_command_terminates_rank() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 1,
            local_rank_count: 1,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Pmi2Server::new(config);
        let socket_path = server.socket_path().to_path_buf();

        let server_handle = tokio::spawn(async move {
            // Abort from a rank causes handle_rank to return Err,
            // which the server logs but continues. With 1 rank,
            // the server loop finishes after that rank's handler completes.
            let _ = server.run(|e| async move { Ok(e) }).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = UnixStream::connect(&socket_path).await.unwrap();

        // fullinit first
        send_recv(&mut stream, "cmd=fullinit;pmi_version=2;\n").await;

        // Send abort — the server won't send a response for abort,
        // but the handler returns, so the connection closes.
        let (_, mut writer) = stream.into_split();
        writer
            .write_all(b"cmd=abort;message=test failure;\n")
            .await
            .unwrap();

        // Server should complete (not hang)
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
        assert!(result.is_ok(), "server should not hang after abort");
    }

    #[tokio::test]
    async fn rank_disconnect_before_finalize() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 1,
            local_rank_count: 1,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Pmi2Server::new(config);
        let socket_path = server.socket_path().to_path_buf();

        let server_handle = tokio::spawn(async move {
            let _ = server.run(|e| async move { Ok(e) }).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = UnixStream::connect(&socket_path).await.unwrap();

        // fullinit, then drop without finalize
        send_recv(&mut stream, "cmd=fullinit;\n").await;
        drop(stream);

        // Server should handle gracefully and not hang
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
        assert!(
            result.is_ok(),
            "server should handle early disconnect gracefully"
        );
    }

    #[tokio::test]
    async fn fence_callback_failure_terminates() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Pmi2ServerConfig {
            launch_id: uuid::Uuid::new_v4(),
            first_rank: 0,
            world_size: 1,
            local_rank_count: 1,
            appnum: 0,
            socket_dir: tmp.path().to_path_buf(),
        };
        let server = Pmi2Server::new(config);
        let socket_path = server.socket_path().to_path_buf();

        let server_handle = tokio::spawn(async move {
            // on_all_fenced returns an error
            server
                .run(|_entries| async move { Err("simulated fence failure".to_string()) })
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = UnixStream::connect(&socket_path).await.unwrap();

        // fullinit
        send_recv(&mut stream, "cmd=fullinit;\n").await;
        // kvsput
        send_recv(&mut stream, "cmd=kvsput;key=k;value=v;\n").await;

        // kvsfence — the callback will fail, which should cause the
        // rank handler to return Err. The connection will close.
        let (reader, mut writer) = stream.into_split();
        writer.write_all(b"cmd=kvsfence;\n").await.unwrap();

        // The rank handler should exit due to the fence error.
        // We may or may not get a response before the handler terminates.
        let mut buf_reader = BufReader::new(reader);
        let mut buf = String::new();
        // Read will either get a response or EOF
        let _ = buf_reader.read_line(&mut buf).await;

        // Server should complete
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server should not hang")
            .expect("server join");

        // The server itself returns Ok(()) because it just logs rank handler errors.
        // The actual error is in the rank handler task.
        assert!(result.is_ok());
    }
}
