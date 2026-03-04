//! Aya-based eBPF collector for kernel-level telemetry.
//!
//! This module provides a real [`EbpfCollector`] implementation that loads
//! pre-compiled BPF object files via the [Aya](https://aya-rs.dev/) library.
//! It is gated behind the `ebpf` feature flag and only compiles on Linux.
//!
//! On non-Linux platforms or without the `ebpf` feature, the agent uses
//! [`ProcSysCollector`](super::proc_collector::ProcSysCollector) instead.

#[cfg(all(target_os = "linux", feature = "ebpf"))]
use super::ebpf_stubs::{CollectorState, EbpfCollector, EbpfEvent};
#[cfg(all(target_os = "linux", feature = "ebpf"))]
use async_trait::async_trait;
#[cfg(all(target_os = "linux", feature = "ebpf"))]
use chrono::Utc;
#[cfg(all(target_os = "linux", feature = "ebpf"))]
use std::path::PathBuf;

/// Configuration for the Aya eBPF collector.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
#[derive(Debug, Clone)]
pub struct AyaCollectorConfig {
    /// Directory containing compiled .bpf.o files.
    pub programs_path: PathBuf,
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
impl Default for AyaCollectorConfig {
    fn default() -> Self {
        Self {
            programs_path: PathBuf::from("/opt/lattice/ebpf"),
        }
    }
}

/// Names of eBPF programs loaded by the collector.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
const PROGRAM_NAMES: &[&str] = &["sched_events", "blk_io", "mem_events", "net_flow"];

/// eBPF collector backed by the Aya library.
///
/// On `attach()`, loads all BPF object files from the configured path and
/// attaches them to their respective tracepoints/kprobes. On `read_events()`,
/// polls the perf event arrays for pending events.
///
/// Requires `CAP_BPF` (or `CAP_SYS_ADMIN` on older kernels).
#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub struct AyaEbpfCollector {
    config: AyaCollectorConfig,
    state: CollectorState,
    bpf_instances: Vec<aya::Ebpf>,
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
impl AyaEbpfCollector {
    pub fn new(config: AyaCollectorConfig) -> Self {
        Self {
            config,
            state: CollectorState::Detached,
            bpf_instances: Vec::new(),
        }
    }

    fn load_program(&mut self, name: &str) -> Result<(), String> {
        let path = self.config.programs_path.join(format!("{name}.bpf.o"));
        let data =
            std::fs::read(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        let mut bpf = aya::Ebpf::load(&data)
            .map_err(|e| format!("failed to load BPF program {name}: {e}"))?;

        // Attach all programs in the object file.
        for (prog_name, prog) in bpf.programs_mut() {
            match prog {
                aya::programs::Program::TracePoint(tp) => {
                    tp.load().map_err(|e| format!("load {prog_name}: {e}"))?;
                    // Category and name are encoded in the section name
                    // e.g., "tp/sched/sched_switch" -> category="sched", name="sched_switch"
                    let parts: Vec<&str> = prog_name.splitn(3, '/').collect();
                    if parts.len() >= 3 {
                        tp.attach(parts[1], parts[2])
                            .map_err(|e| format!("attach {prog_name}: {e}"))?;
                    }
                }
                aya::programs::Program::KProbe(kp) => {
                    kp.load().map_err(|e| format!("load {prog_name}: {e}"))?;
                    let fn_name = prog_name.strip_prefix("kprobe/").unwrap_or(prog_name);
                    kp.attach(fn_name, 0)
                        .map_err(|e| format!("attach {prog_name}: {e}"))?;
                }
                _ => {
                    tracing::warn!(program = prog_name, "skipping unsupported program type");
                }
            }
        }

        self.bpf_instances.push(bpf);
        Ok(())
    }
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
#[async_trait]
impl EbpfCollector for AyaEbpfCollector {
    async fn attach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Attached {
            return Err("already attached".to_string());
        }

        for name in PROGRAM_NAMES {
            if let Err(e) = self.load_program(name) {
                tracing::warn!(program = name, error = %e, "failed to load eBPF program (continuing)");
            }
        }

        self.state = CollectorState::Attached;
        Ok(())
    }

    async fn detach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Detached {
            return Err("not attached".to_string());
        }
        // Dropping Bpf instances detaches programs.
        self.bpf_instances.clear();
        self.state = CollectorState::Detached;
        Ok(())
    }

    async fn read_events(&mut self) -> Result<Vec<EbpfEvent>, String> {
        if self.state != CollectorState::Attached {
            return Err("collector is not attached".to_string());
        }

        let mut events = Vec::new();

        // Poll perf event arrays from each loaded BPF instance.
        for bpf in &mut self.bpf_instances {
            let map_names: Vec<String> = bpf
                .maps()
                .filter(|(name, _)| *name == "events")
                .map(|(name, _)| name.to_string())
                .collect();

            for map_name in &map_names {
                let map = match bpf.map_mut(map_name) {
                    Some(m) => m,
                    None => continue,
                };
                if let Ok(mut perf_array) = aya::maps::PerfEventArray::try_from(map) {
                    // Read available buffers from each CPU.
                    let cpus = aya::util::online_cpus()
                        .map_err(|(msg, e)| format!("failed to get online CPUs: {msg}: {e}"))?;
                    for cpu in cpus {
                        let mut buf = [0u8; 4096];
                        if let Ok(mut perf_buf) = perf_array.open(cpu, Some(64)) {
                            while let Ok(event) = perf_buf.read_events(&mut [&mut buf]) {
                                if event.read == 0 {
                                    break;
                                }
                                events.push(EbpfEvent {
                                    timestamp: Utc::now(),
                                    kind: map_name.to_string(),
                                    payload: buf[..event.read].to_vec(),
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(events)
    }

    fn state(&self) -> CollectorState {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Stub for non-Linux / non-ebpf builds (event parsing helpers only)
// ---------------------------------------------------------------------------

/// Parse a raw sched_event from eBPF perf buffer bytes.
///
/// Layout: timestamp_ns(u64) + cpu(u32) + prev_pid(u32) + next_pid(u32) + latency_ns(u64)
pub fn parse_sched_event(data: &[u8]) -> Option<SchedEventParsed> {
    if data.len() < 28 {
        return None;
    }
    Some(SchedEventParsed {
        timestamp_ns: u64::from_ne_bytes(data[0..8].try_into().ok()?),
        cpu: u32::from_ne_bytes(data[8..12].try_into().ok()?),
        prev_pid: u32::from_ne_bytes(data[12..16].try_into().ok()?),
        next_pid: u32::from_ne_bytes(data[16..20].try_into().ok()?),
        latency_ns: u64::from_ne_bytes(data[20..28].try_into().ok()?),
    })
}

/// Parsed scheduling event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedEventParsed {
    pub timestamp_ns: u64,
    pub cpu: u32,
    pub prev_pid: u32,
    pub next_pid: u32,
    pub latency_ns: u64,
}

/// Parse a raw io_event from eBPF perf buffer bytes.
///
/// Layout: timestamp_ns(u64) + dev_major(u32) + dev_minor(u32) + latency_ns(u64) + data_len(u32) + is_write(u8)
pub fn parse_io_event(data: &[u8]) -> Option<IoEventParsed> {
    if data.len() < 29 {
        return None;
    }
    Some(IoEventParsed {
        timestamp_ns: u64::from_ne_bytes(data[0..8].try_into().ok()?),
        dev_major: u32::from_ne_bytes(data[8..12].try_into().ok()?),
        dev_minor: u32::from_ne_bytes(data[12..16].try_into().ok()?),
        latency_ns: u64::from_ne_bytes(data[16..24].try_into().ok()?),
        data_len: u32::from_ne_bytes(data[24..28].try_into().ok()?),
        is_write: data[28] != 0,
    })
}

/// Parsed I/O event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoEventParsed {
    pub timestamp_ns: u64,
    pub dev_major: u32,
    pub dev_minor: u32,
    pub latency_ns: u64,
    pub data_len: u32,
    pub is_write: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sched_event_valid() {
        let mut data = Vec::new();
        data.extend_from_slice(&1000u64.to_ne_bytes()); // timestamp
        data.extend_from_slice(&3u32.to_ne_bytes()); // cpu
        data.extend_from_slice(&100u32.to_ne_bytes()); // prev_pid
        data.extend_from_slice(&200u32.to_ne_bytes()); // next_pid
        data.extend_from_slice(&5000u64.to_ne_bytes()); // latency

        let evt = parse_sched_event(&data).unwrap();
        assert_eq!(evt.timestamp_ns, 1000);
        assert_eq!(evt.cpu, 3);
        assert_eq!(evt.prev_pid, 100);
        assert_eq!(evt.next_pid, 200);
        assert_eq!(evt.latency_ns, 5000);
    }

    #[test]
    fn parse_sched_event_too_short() {
        let data = [0u8; 10];
        assert!(parse_sched_event(&data).is_none());
    }

    #[test]
    fn parse_io_event_valid() {
        let mut data = Vec::new();
        data.extend_from_slice(&2000u64.to_ne_bytes()); // timestamp
        data.extend_from_slice(&8u32.to_ne_bytes()); // dev_major
        data.extend_from_slice(&0u32.to_ne_bytes()); // dev_minor
        data.extend_from_slice(&10000u64.to_ne_bytes()); // latency
        data.extend_from_slice(&4096u32.to_ne_bytes()); // data_len
        data.push(1); // is_write

        let evt = parse_io_event(&data).unwrap();
        assert_eq!(evt.timestamp_ns, 2000);
        assert_eq!(evt.dev_major, 8);
        assert_eq!(evt.dev_minor, 0);
        assert_eq!(evt.latency_ns, 10000);
        assert_eq!(evt.data_len, 4096);
        assert!(evt.is_write);
    }

    #[test]
    fn parse_io_event_read() {
        let mut data = Vec::new();
        data.extend_from_slice(&0u64.to_ne_bytes());
        data.extend_from_slice(&0u32.to_ne_bytes());
        data.extend_from_slice(&0u32.to_ne_bytes());
        data.extend_from_slice(&0u64.to_ne_bytes());
        data.extend_from_slice(&512u32.to_ne_bytes());
        data.push(0); // is_write = false

        let evt = parse_io_event(&data).unwrap();
        assert!(!evt.is_write);
        assert_eq!(evt.data_len, 512);
    }

    #[test]
    fn parse_io_event_too_short() {
        let data = [0u8; 20];
        assert!(parse_io_event(&data).is_none());
    }

    #[test]
    fn parse_sched_event_empty() {
        assert!(parse_sched_event(&[]).is_none());
    }
}
