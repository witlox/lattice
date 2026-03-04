//! /proc and /sys based telemetry collector.
//!
//! Parses /proc/stat, /proc/meminfo, /proc/net/dev for system metrics,
//! and shells out to nvidia-smi / rocm-smi for GPU metrics.
//!
//! All parsing logic is implemented as pure functions that operate on string
//! slices, making them fully testable on any platform (including macOS).
//! The [`ProcSysCollector`] struct reads from the real filesystem on Linux
//! and returns safe defaults on other platforms.

use super::ebpf_stubs::{CollectorState, EbpfCollector, EbpfEvent};
use async_trait::async_trait;
use chrono::Utc;

// ---------------------------------------------------------------------------
// Metric structs
// ---------------------------------------------------------------------------

/// CPU utilization metrics parsed from /proc/stat.
#[derive(Debug, Clone, Default)]
pub struct CpuMetrics {
    /// Percentage of time spent in user mode (user + nice).
    pub user_percent: f64,
    /// Percentage of time spent in kernel mode (system + irq + softirq).
    pub system_percent: f64,
    /// Percentage of time spent idle (idle + iowait).
    pub idle_percent: f64,
    /// Overall utilization (100 - idle_percent).
    pub total_percent: f64,
}

/// Memory utilization metrics parsed from /proc/meminfo.
#[derive(Debug, Clone, Default)]
pub struct MemoryMetrics {
    /// Total physical memory in kB.
    pub total_kb: u64,
    /// Available memory in kB.
    pub available_kb: u64,
    /// Used memory in kB (total - available).
    pub used_kb: u64,
    /// Memory usage as a percentage.
    pub usage_percent: f64,
}

/// Per-interface network counters parsed from /proc/net/dev.
#[derive(Debug, Clone, Default)]
pub struct NetworkMetrics {
    /// Interface name (e.g. "eth0").
    pub interface: String,
    /// Total bytes received.
    pub rx_bytes: u64,
    /// Total bytes transmitted.
    pub tx_bytes: u64,
    /// Total packets received.
    pub rx_packets: u64,
    /// Total packets transmitted.
    pub tx_packets: u64,
}

/// GPU utilization metrics from nvidia-smi or rocm-smi.
#[derive(Debug, Clone, Default)]
pub struct GpuMetrics {
    /// GPU device index.
    pub index: u32,
    /// GPU compute utilization percentage.
    pub utilization_percent: f64,
    /// GPU memory used in MB.
    pub memory_used_mb: f64,
    /// GPU total memory in MB.
    pub memory_total_mb: f64,
    /// Current power draw in watts.
    pub power_draw_watts: f64,
    /// GPU temperature in Celsius.
    pub temperature_celsius: f64,
}

/// Scheduler and context switch metrics parsed from /proc/stat.
#[derive(Debug, Clone, Default)]
pub struct SchedulerMetrics {
    /// Total context switches since boot.
    pub context_switches: u64,
    /// Total processes created since boot.
    pub processes_created: u64,
    /// Currently running processes.
    pub procs_running: u64,
    /// Currently blocked processes.
    pub procs_blocked: u64,
}

/// Per-device disk I/O metrics parsed from /proc/diskstats.
#[derive(Debug, Clone, Default)]
pub struct DiskIoMetrics {
    /// Device name (e.g. "sda", "nvme0n1").
    pub device: String,
    /// Number of reads completed.
    pub reads_completed: u64,
    /// Number of sectors read.
    pub sectors_read: u64,
    /// Time spent reading (ms).
    pub read_time_ms: u64,
    /// Number of writes completed.
    pub writes_completed: u64,
    /// Number of sectors written.
    pub sectors_written: u64,
    /// Time spent writing (ms).
    pub write_time_ms: u64,
    /// Current I/Os in progress.
    pub ios_in_progress: u64,
}

/// Virtual memory statistics parsed from /proc/vmstat.
#[derive(Debug, Clone, Default)]
pub struct VmStatMetrics {
    /// Page faults (minor).
    pub pgfault: u64,
    /// Page faults (major).
    pub pgmajfault: u64,
    /// Pages allocated (normal zone).
    pub pgalloc_normal: u64,
    /// Pages freed.
    pub pgfree: u64,
    /// Pages paged in from disk.
    pub pgpgin: u64,
    /// Pages paged out to disk.
    pub pgpgout: u64,
    /// Pages swapped in.
    pub pswpin: u64,
    /// Pages swapped out.
    pub pswpout: u64,
    /// NUMA local allocations.
    pub numa_local: u64,
    /// NUMA foreign allocations.
    pub numa_foreign: u64,
}

/// TCP connection state counters parsed from /proc/net/tcp.
#[derive(Debug, Clone, Default)]
pub struct TcpConnectionMetrics {
    /// Connections in ESTABLISHED state.
    pub established: u64,
    /// Connections in TIME_WAIT state.
    pub time_wait: u64,
    /// Connections in CLOSE_WAIT state.
    pub close_wait: u64,
    /// Total connections.
    pub total: u64,
}

/// A point-in-time snapshot of all system metrics.
#[derive(Debug, Clone, Default)]
pub struct SystemSnapshot {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub network: Vec<NetworkMetrics>,
    pub gpus: Vec<GpuMetrics>,
    pub scheduler: SchedulerMetrics,
    pub disk_io: Vec<DiskIoMetrics>,
    pub vmstat: VmStatMetrics,
    pub tcp: TcpConnectionMetrics,
}

// ---------------------------------------------------------------------------
// Pure parsing functions
// ---------------------------------------------------------------------------

/// Parse the aggregate CPU line from /proc/stat.
///
/// The first line has the format:
/// ```text
/// cpu  <user> <nice> <system> <idle> <iowait> <irq> <softirq> <steal> [guest] [guest_nice]
/// ```
/// All values are in jiffies. We compute percentages relative to the total.
pub fn parse_proc_stat(content: &str) -> CpuMetrics {
    let Some(line) = content.lines().find(|l| l.starts_with("cpu ")) else {
        return CpuMetrics::default();
    };

    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1) // skip "cpu"
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();

    // Need at least: user, nice, system, idle, iowait, irq, softirq, steal
    if fields.len() < 8 {
        return CpuMetrics::default();
    }

    let user = fields[0];
    let nice = fields[1];
    let system = fields[2];
    let idle = fields[3];
    let iowait = fields[4];
    let irq = fields[5];
    let softirq = fields[6];
    let steal = fields[7];

    let total = (user + nice + system + idle + iowait + irq + softirq + steal) as f64;
    if total == 0.0 {
        return CpuMetrics::default();
    }

    let user_pct = (user + nice) as f64 / total * 100.0;
    let system_pct = (system + irq + softirq) as f64 / total * 100.0;
    let idle_pct = (idle + iowait) as f64 / total * 100.0;

    CpuMetrics {
        user_percent: user_pct,
        system_percent: system_pct,
        idle_percent: idle_pct,
        total_percent: 100.0 - idle_pct,
    }
}

/// Parse /proc/meminfo for total and available memory.
///
/// Looks for `MemTotal:` and `MemAvailable:` lines.
pub fn parse_proc_meminfo(content: &str) -> MemoryMetrics {
    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;

    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = extract_meminfo_value(line);
        } else if line.starts_with("MemAvailable:") {
            available_kb = extract_meminfo_value(line);
        }
        // Stop early once we have both.
        if total_kb.is_some() && available_kb.is_some() {
            break;
        }
    }

    let total = total_kb.unwrap_or(0);
    let available = available_kb.unwrap_or(0);
    let used = total.saturating_sub(available);
    let usage_percent = if total > 0 {
        used as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    MemoryMetrics {
        total_kb: total,
        available_kb: available,
        used_kb: used,
        usage_percent,
    }
}

/// Extract the numeric value from a meminfo line like "MemTotal:  16384000 kB".
fn extract_meminfo_value(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse::<u64>().ok()
}

/// Parse /proc/net/dev for per-interface network counters.
///
/// The file has two header lines followed by per-interface lines:
/// ```text
///   iface: rx_bytes rx_packets rx_errs rx_drop rx_fifo rx_frame rx_compressed rx_multicast
///          tx_bytes tx_packets tx_errs tx_drop tx_fifo tx_colls tx_carrier tx_compressed
/// ```
/// The loopback interface (`lo`) is skipped.
pub fn parse_proc_net_dev(content: &str) -> Vec<NetworkMetrics> {
    let mut result = Vec::new();

    for line in content.lines().skip(2) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };

        let iface = iface.trim();
        if iface == "lo" {
            continue;
        }

        let fields: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();

        // We need at least 10 fields (rx: 8, tx: first 2).
        if fields.len() < 10 {
            continue;
        }

        result.push(NetworkMetrics {
            interface: iface.to_string(),
            rx_bytes: fields[0],
            rx_packets: fields[1],
            tx_bytes: fields[8],
            tx_packets: fields[9],
        });
    }

    result
}

/// Parse CSV output from `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`.
///
/// Expected columns: utilization.gpu, memory.used, memory.total, power.draw, temperature.gpu
/// Each line looks like: `85, 4096, 8192, 250.00, 72`
pub fn parse_nvidia_smi(output: &str) -> Vec<GpuMetrics> {
    let mut result = Vec::new();

    for (idx, line) in output.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < 5 {
            continue;
        }

        let util = match fields[0].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mem_used = match fields[1].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mem_total = match fields[2].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let power = match fields[3].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let temp = match fields[4].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        result.push(GpuMetrics {
            index: idx as u32,
            utilization_percent: util,
            memory_used_mb: mem_used,
            memory_total_mb: mem_total,
            power_draw_watts: power,
            temperature_celsius: temp,
        });
    }

    result
}

/// Parse CSV output from `rocm-smi --showuse --showmemuse --showtemp --csv`.
///
/// Expected header: `device,GPU use (%),GPU memory use (%),Temperature (Sensor edge) (C)`
/// Data lines: `0,85.0,50.0,72.0`
///
/// Note: rocm-smi reports memory as a percentage, not absolute. We set
/// `memory_total_mb` to 0 and store the percentage in `memory_used_mb`.
pub fn parse_rocm_smi(output: &str) -> Vec<GpuMetrics> {
    let mut result = Vec::new();
    let mut lines = output.lines();

    // Skip the header line.
    let _header = lines.next();

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < 4 {
            continue;
        }

        let device = match fields[0].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let util = match fields[1].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mem_pct = match fields[2].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let temp = match fields[3].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        result.push(GpuMetrics {
            index: device,
            utilization_percent: util,
            memory_used_mb: mem_pct, // percentage, not MB
            memory_total_mb: 0.0,    // unknown from rocm-smi
            power_draw_watts: 0.0,   // not reported in this query
            temperature_celsius: temp,
        });
    }

    result
}

/// Parse scheduler-related counters from /proc/stat.
///
/// Looks for lines starting with `ctxt`, `processes`, `procs_running`, `procs_blocked`.
pub fn parse_proc_stat_scheduler(content: &str) -> SchedulerMetrics {
    let mut metrics = SchedulerMetrics::default();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("ctxt") => {
                metrics.context_switches = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            Some("processes") => {
                metrics.processes_created = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            Some("procs_running") => {
                metrics.procs_running = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            Some("procs_blocked") => {
                metrics.procs_blocked = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            _ => {}
        }
    }
    metrics
}

/// Parse /proc/diskstats for per-device I/O counters.
///
/// Format: `major minor device reads_completed reads_merged sectors_read read_time
///          writes_completed writes_merged sectors_written write_time ios_in_progress ...`
///
/// Skips loop devices (loop*) and ram devices (ram*).
pub fn parse_proc_diskstats(content: &str) -> Vec<DiskIoMetrics> {
    let mut result = Vec::new();
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 14 {
            continue;
        }
        let device = fields[2];
        // Skip virtual devices
        if device.starts_with("loop") || device.starts_with("ram") || device.starts_with("dm-") {
            continue;
        }

        let parse = |i: usize| -> u64 { fields[i].parse().unwrap_or(0) };

        result.push(DiskIoMetrics {
            device: device.to_string(),
            reads_completed: parse(3),
            sectors_read: parse(5),
            read_time_ms: parse(6),
            writes_completed: parse(7),
            sectors_written: parse(9),
            write_time_ms: parse(10),
            ios_in_progress: parse(11),
        });
    }
    result
}

/// Parse /proc/vmstat for virtual memory statistics.
///
/// Each line is `key value`. We extract a known set of keys.
pub fn parse_proc_vmstat(content: &str) -> VmStatMetrics {
    let mut metrics = VmStatMetrics::default();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let key = match parts.next() {
            Some(k) => k,
            None => continue,
        };
        let val: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        match key {
            "pgfault" => metrics.pgfault = val,
            "pgmajfault" => metrics.pgmajfault = val,
            "pgalloc_normal" => metrics.pgalloc_normal = val,
            "pgfree" => metrics.pgfree = val,
            "pgpgin" => metrics.pgpgin = val,
            "pgpgout" => metrics.pgpgout = val,
            "pswpin" => metrics.pswpin = val,
            "pswpout" => metrics.pswpout = val,
            "numa_local" => metrics.numa_local = val,
            "numa_foreign" => metrics.numa_foreign = val,
            _ => {}
        }
    }
    metrics
}

/// Parse /proc/net/tcp for TCP connection state counters.
///
/// Each line after the header has `sl local_address rem_address st ...`
/// where `st` is the TCP state as a 2-digit hex code.
///
/// Key states:
/// - 01 = ESTABLISHED
/// - 06 = TIME_WAIT
/// - 08 = CLOSE_WAIT
pub fn parse_proc_net_tcp(content: &str) -> TcpConnectionMetrics {
    let mut metrics = TcpConnectionMetrics::default();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        metrics.total += 1;
        match fields[3] {
            "01" => metrics.established += 1,
            "06" => metrics.time_wait += 1,
            "08" => metrics.close_wait += 1,
            _ => {}
        }
    }
    metrics
}

// ---------------------------------------------------------------------------
// ProcSysCollector
// ---------------------------------------------------------------------------

/// A telemetry collector that reads system metrics from /proc and /sys.
///
/// On Linux it reads the real proc filesystem; on other platforms every
/// read method returns safe defaults so the collector can be used in tests
/// and development without conditional compilation at the call site.
pub struct ProcSysCollector {
    state: CollectorState,
}

impl ProcSysCollector {
    /// Create a new collector in the detached state.
    pub fn new() -> Self {
        Self {
            state: CollectorState::Detached,
        }
    }

    /// Collect a point-in-time snapshot of all system metrics.
    ///
    /// On non-Linux platforms this returns default (zero) values.
    pub async fn collect(&self) -> SystemSnapshot {
        let cpu = self.read_cpu().await;
        let memory = self.read_memory().await;
        let network = self.read_network().await;
        let gpus = self.read_gpus().await;
        let scheduler = self.read_scheduler().await;
        let disk_io = self.read_disk_io().await;
        let vmstat = self.read_vmstat().await;
        let tcp = self.read_tcp().await;
        SystemSnapshot {
            cpu,
            memory,
            network,
            gpus,
            scheduler,
            disk_io,
            vmstat,
            tcp,
        }
    }

    // -- CPU ----------------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_cpu(&self) -> CpuMetrics {
        match tokio::fs::read_to_string("/proc/stat").await {
            Ok(content) => parse_proc_stat(&content),
            Err(_) => CpuMetrics::default(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_cpu(&self) -> CpuMetrics {
        CpuMetrics::default()
    }

    // -- Memory -------------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_memory(&self) -> MemoryMetrics {
        match tokio::fs::read_to_string("/proc/meminfo").await {
            Ok(content) => parse_proc_meminfo(&content),
            Err(_) => MemoryMetrics::default(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_memory(&self) -> MemoryMetrics {
        MemoryMetrics::default()
    }

    // -- Network ------------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_network(&self) -> Vec<NetworkMetrics> {
        match tokio::fs::read_to_string("/proc/net/dev").await {
            Ok(content) => parse_proc_net_dev(&content),
            Err(_) => Vec::new(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_network(&self) -> Vec<NetworkMetrics> {
        Vec::new()
    }

    // -- GPUs ---------------------------------------------------------------

    /// Try nvidia-smi first, then rocm-smi. Return empty if neither is available.
    async fn read_gpus(&self) -> Vec<GpuMetrics> {
        // Try NVIDIA first.
        if let Ok(output) = tokio::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=utilization.gpu,memory.used,memory.total,power.draw,temperature.gpu",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let gpus = parse_nvidia_smi(&stdout);
                if !gpus.is_empty() {
                    return gpus;
                }
            }
        }

        // Fallback to AMD ROCm.
        if let Ok(output) = tokio::process::Command::new("rocm-smi")
            .args(["--showuse", "--showmemuse", "--showtemp", "--csv"])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return parse_rocm_smi(&stdout);
            }
        }

        Vec::new()
    }

    // -- Scheduler ----------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_scheduler(&self) -> SchedulerMetrics {
        match tokio::fs::read_to_string("/proc/stat").await {
            Ok(content) => parse_proc_stat_scheduler(&content),
            Err(_) => SchedulerMetrics::default(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_scheduler(&self) -> SchedulerMetrics {
        SchedulerMetrics::default()
    }

    // -- Disk I/O -----------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_disk_io(&self) -> Vec<DiskIoMetrics> {
        match tokio::fs::read_to_string("/proc/diskstats").await {
            Ok(content) => parse_proc_diskstats(&content),
            Err(_) => Vec::new(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_disk_io(&self) -> Vec<DiskIoMetrics> {
        Vec::new()
    }

    // -- vmstat -------------------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_vmstat(&self) -> VmStatMetrics {
        match tokio::fs::read_to_string("/proc/vmstat").await {
            Ok(content) => parse_proc_vmstat(&content),
            Err(_) => VmStatMetrics::default(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_vmstat(&self) -> VmStatMetrics {
        VmStatMetrics::default()
    }

    // -- TCP connections ----------------------------------------------------

    #[cfg(target_os = "linux")]
    async fn read_tcp(&self) -> TcpConnectionMetrics {
        match tokio::fs::read_to_string("/proc/net/tcp").await {
            Ok(content) => parse_proc_net_tcp(&content),
            Err(_) => TcpConnectionMetrics::default(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_tcp(&self) -> TcpConnectionMetrics {
        TcpConnectionMetrics::default()
    }
}

impl Default for ProcSysCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// EbpfCollector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl EbpfCollector for ProcSysCollector {
    async fn attach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Attached {
            return Err("already attached".to_string());
        }
        self.state = CollectorState::Attached;
        Ok(())
    }

    async fn detach(&mut self) -> Result<(), String> {
        if self.state == CollectorState::Detached {
            return Err("not attached".to_string());
        }
        self.state = CollectorState::Detached;
        Ok(())
    }

    async fn read_events(&mut self) -> Result<Vec<EbpfEvent>, String> {
        if self.state != CollectorState::Attached {
            return Err("collector is not attached".to_string());
        }

        let snapshot = self.collect().await;

        let payload = serde_json::to_vec(&serde_json::json!({
            "cpu": {
                "user_percent": snapshot.cpu.user_percent,
                "system_percent": snapshot.cpu.system_percent,
                "total_percent": snapshot.cpu.total_percent,
            },
            "memory": {
                "total_kb": snapshot.memory.total_kb,
                "used_kb": snapshot.memory.used_kb,
                "usage_percent": snapshot.memory.usage_percent,
            },
            "network_interfaces": snapshot.network.len(),
            "gpus": snapshot.gpus.len(),
            "scheduler": {
                "context_switches": snapshot.scheduler.context_switches,
                "processes_created": snapshot.scheduler.processes_created,
                "procs_running": snapshot.scheduler.procs_running,
                "procs_blocked": snapshot.scheduler.procs_blocked,
            },
            "disk_io_devices": snapshot.disk_io.len(),
            "vmstat": {
                "pgfault": snapshot.vmstat.pgfault,
                "pgmajfault": snapshot.vmstat.pgmajfault,
                "pswpin": snapshot.vmstat.pswpin,
                "pswpout": snapshot.vmstat.pswpout,
            },
            "tcp": {
                "established": snapshot.tcp.established,
                "time_wait": snapshot.tcp.time_wait,
                "close_wait": snapshot.tcp.close_wait,
                "total": snapshot.tcp.total,
            },
        }))
        .unwrap_or_default();

        Ok(vec![EbpfEvent {
            timestamp: Utc::now(),
            kind: "system_snapshot".to_string(),
            payload,
        }])
    }

    fn state(&self) -> CollectorState {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Sample data --------------------------------------------------------

    const SAMPLE_PROC_STAT: &str = "\
cpu  10132153 290696 3084719 46828483 16683 0 25195 0 0 0
cpu0 5066076 145348 1542359 23414241 8341 0 12597 0 0 0
cpu1 5066077 145348 1542360 23414242 8342 0 12598 0 0 0";

    const SAMPLE_MEMINFO: &str = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
MemAvailable:    8192000 kB
Buffers:          512000 kB
Cached:          4096000 kB
SwapCached:            0 kB";

    const SAMPLE_NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1234567    5678    0    0    0     0          0         0  1234567    5678    0    0    0     0       0          0
  eth0: 98765432   12345    0    0    0     0          0         0 87654321   11111    0    0    0     0       0          0
";

    const SAMPLE_NVIDIA_SMI: &str = "\
85, 4096, 8192, 250.00, 72
92, 6144, 8192, 275.50, 78
";

    const SAMPLE_ROCM_SMI: &str = "\
device,GPU use (%),GPU memory use (%),Temperature (Sensor edge) (C)
0,85.0,50.0,72.0
1,92.0,75.0,78.0
";

    // -- parse_proc_stat tests ----------------------------------------------

    #[test]
    fn test_parse_proc_stat() {
        let cpu = parse_proc_stat(SAMPLE_PROC_STAT);

        // total jiffies = 10132153 + 290696 + 3084719 + 46828483 + 16683 + 0 + 25195 + 0 = 60377929
        let total: f64 = 60_377_929.0;
        let expected_user = (10_132_153.0 + 290_696.0) / total * 100.0;
        let expected_system = (3_084_719.0 + 0.0 + 25_195.0) / total * 100.0;
        let expected_idle = (46_828_483.0 + 16_683.0) / total * 100.0;

        assert!(
            (cpu.user_percent - expected_user).abs() < 0.01,
            "user: got {}, expected {}",
            cpu.user_percent,
            expected_user
        );
        assert!(
            (cpu.system_percent - expected_system).abs() < 0.01,
            "system: got {}, expected {}",
            cpu.system_percent,
            expected_system
        );
        assert!(
            (cpu.idle_percent - expected_idle).abs() < 0.01,
            "idle: got {}, expected {}",
            cpu.idle_percent,
            expected_idle
        );
        assert!(
            (cpu.total_percent - (100.0 - expected_idle)).abs() < 0.01,
            "total: got {}, expected {}",
            cpu.total_percent,
            100.0 - expected_idle
        );
    }

    #[test]
    fn test_parse_proc_stat_percentages_sum_to_100() {
        let cpu = parse_proc_stat(SAMPLE_PROC_STAT);
        let sum = cpu.user_percent + cpu.system_percent + cpu.idle_percent;
        // There may be steal time that is neither user/system/idle, but in our
        // sample steal=0, so user+system+idle should approximately equal 100%.
        assert!((sum - 100.0).abs() < 0.01, "sum of percentages: {}", sum);
    }

    // -- parse_proc_meminfo tests -------------------------------------------

    #[test]
    fn test_parse_proc_meminfo() {
        let mem = parse_proc_meminfo(SAMPLE_MEMINFO);

        assert_eq!(mem.total_kb, 16_384_000);
        assert_eq!(mem.available_kb, 8_192_000);
        assert_eq!(mem.used_kb, 16_384_000 - 8_192_000);

        let expected_pct = (16_384_000.0 - 8_192_000.0) / 16_384_000.0 * 100.0;
        assert!(
            (mem.usage_percent - expected_pct).abs() < 0.01,
            "usage_percent: got {}, expected {}",
            mem.usage_percent,
            expected_pct
        );
        // 50% usage
        assert!((mem.usage_percent - 50.0).abs() < 0.01);
    }

    // -- parse_proc_net_dev tests -------------------------------------------

    #[test]
    fn test_parse_proc_net_dev() {
        let nets = parse_proc_net_dev(SAMPLE_NET_DEV);

        assert_eq!(nets.len(), 1, "loopback should be skipped");
        assert_eq!(nets[0].interface, "eth0");
        assert_eq!(nets[0].rx_bytes, 98_765_432);
        assert_eq!(nets[0].tx_bytes, 87_654_321);
        assert_eq!(nets[0].rx_packets, 12_345);
        assert_eq!(nets[0].tx_packets, 11_111);
    }

    #[test]
    fn test_net_dev_skips_loopback() {
        let content = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1234567    5678    0    0    0     0          0         0  1234567    5678    0    0    0     0       0          0
";
        let nets = parse_proc_net_dev(content);
        assert!(nets.is_empty(), "only loopback present, should be empty");
    }

    #[test]
    fn test_net_dev_multiple_interfaces() {
        let content = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 100    10    0    0    0     0          0         0  100    10    0    0    0     0       0          0
  eth0: 200    20    0    0    0     0          0         0  300    30    0    0    0     0       0          0
 wlan0: 400    40    0    0    0     0          0         0  500    50    0    0    0     0       0          0
";
        let nets = parse_proc_net_dev(content);
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].interface, "eth0");
        assert_eq!(nets[1].interface, "wlan0");
    }

    // -- parse_nvidia_smi tests ---------------------------------------------

    #[test]
    fn test_parse_nvidia_smi() {
        let gpus = parse_nvidia_smi(SAMPLE_NVIDIA_SMI);

        assert_eq!(gpus.len(), 2);

        assert_eq!(gpus[0].index, 0);
        assert!((gpus[0].utilization_percent - 85.0).abs() < 0.01);
        assert!((gpus[0].memory_used_mb - 4096.0).abs() < 0.01);
        assert!((gpus[0].memory_total_mb - 8192.0).abs() < 0.01);
        assert!((gpus[0].power_draw_watts - 250.0).abs() < 0.01);
        assert!((gpus[0].temperature_celsius - 72.0).abs() < 0.01);

        assert_eq!(gpus[1].index, 1);
        assert!((gpus[1].utilization_percent - 92.0).abs() < 0.01);
        assert!((gpus[1].memory_used_mb - 6144.0).abs() < 0.01);
        assert!((gpus[1].power_draw_watts - 275.5).abs() < 0.01);
        assert!((gpus[1].temperature_celsius - 78.0).abs() < 0.01);
    }

    // -- parse_rocm_smi tests -----------------------------------------------

    #[test]
    fn test_parse_rocm_smi() {
        let gpus = parse_rocm_smi(SAMPLE_ROCM_SMI);

        assert_eq!(gpus.len(), 2);

        assert_eq!(gpus[0].index, 0);
        assert!((gpus[0].utilization_percent - 85.0).abs() < 0.01);
        assert!((gpus[0].memory_used_mb - 50.0).abs() < 0.01); // percentage stored here
        assert!((gpus[0].memory_total_mb - 0.0).abs() < 0.01); // unknown
        assert!((gpus[0].power_draw_watts - 0.0).abs() < 0.01);
        assert!((gpus[0].temperature_celsius - 72.0).abs() < 0.01);

        assert_eq!(gpus[1].index, 1);
        assert!((gpus[1].utilization_percent - 92.0).abs() < 0.01);
        assert!((gpus[1].memory_used_mb - 75.0).abs() < 0.01);
        assert!((gpus[1].temperature_celsius - 78.0).abs() < 0.01);
    }

    // -- Empty / malformed input tests --------------------------------------

    #[test]
    fn test_empty_input_returns_defaults() {
        let cpu = parse_proc_stat("");
        assert!((cpu.total_percent - 0.0).abs() < 0.001);

        let mem = parse_proc_meminfo("");
        assert_eq!(mem.total_kb, 0);
        assert_eq!(mem.used_kb, 0);

        let nets = parse_proc_net_dev("");
        assert!(nets.is_empty());

        let nvidia = parse_nvidia_smi("");
        assert!(nvidia.is_empty());

        let rocm = parse_rocm_smi("");
        assert!(rocm.is_empty());
    }

    #[test]
    fn test_malformed_proc_stat_returns_default() {
        let cpu = parse_proc_stat("garbage data\nnot a cpu line");
        assert!((cpu.total_percent - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_nvidia_smi_partial_line_skipped() {
        let output = "85, 4096\n92, 6144, 8192, 275.50, 78\n";
        let gpus = parse_nvidia_smi(output);
        // First line has only 2 fields, should be skipped.
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].index, 1); // second non-empty line, index=1
    }

    // -- Collector lifecycle tests ------------------------------------------

    #[tokio::test]
    async fn test_collector_new_is_detached() {
        let collector = ProcSysCollector::new();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    #[tokio::test]
    async fn test_collector_attach_detach() {
        let mut collector = ProcSysCollector::new();

        collector.attach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Attached);

        collector.detach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    #[tokio::test]
    async fn test_collector_double_attach_fails() {
        let mut collector = ProcSysCollector::new();
        collector.attach().await.unwrap();
        let result = collector.attach().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "already attached");
    }

    #[tokio::test]
    async fn test_collector_detach_when_detached_fails() {
        let mut collector = ProcSysCollector::new();
        let result = collector.detach().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "not attached");
    }

    #[tokio::test]
    async fn test_collector_read_events_when_detached() {
        let mut collector = ProcSysCollector::new();
        let result = collector.read_events().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "collector is not attached");
    }

    #[tokio::test]
    async fn test_collector_read_events_when_attached() {
        let mut collector = ProcSysCollector::new();
        collector.attach().await.unwrap();

        let events = collector.read_events().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "system_snapshot");

        // Verify the payload is valid JSON.
        let payload: serde_json::Value =
            serde_json::from_slice(&events[0].payload).expect("payload should be valid JSON");
        assert!(payload.get("cpu").is_some());
        assert!(payload.get("memory").is_some());
        assert!(payload.get("gpus").is_some());
    }

    #[tokio::test]
    async fn test_collector_reattach() {
        let mut collector = ProcSysCollector::new();
        collector.attach().await.unwrap();
        collector.detach().await.unwrap();
        collector.attach().await.unwrap();
        assert_eq!(collector.state(), CollectorState::Attached);

        // Should still produce events after reattach.
        let events = collector.read_events().await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_collector_collect_returns_snapshot() {
        // On macOS this returns defaults; on Linux it reads real data.
        // Either way it should not panic.
        let collector = ProcSysCollector::new();
        let snapshot = collector.collect().await;

        // Basic structural checks -- always valid.
        assert!(snapshot.cpu.idle_percent >= 0.0);
        assert!(snapshot.cpu.total_percent >= 0.0);
        assert!(snapshot.memory.usage_percent >= 0.0);
    }

    #[test]
    fn test_default_trait() {
        let collector = ProcSysCollector::default();
        assert_eq!(collector.state(), CollectorState::Detached);
    }

    // -- parse_proc_stat_scheduler tests ------------------------------------

    #[test]
    fn test_parse_proc_stat_scheduler() {
        let content = "\
cpu  10132153 290696 3084719 46828483 16683 0 25195 0 0 0
cpu0 5066076 145348 1542359 23414241 8341 0 12597 0 0 0
intr 12345678 0 0 0 0 0 0 0
ctxt 987654321
btime 1700000000
processes 123456
procs_running 3
procs_blocked 1
softirq 5432100 0 0 0 0 0 0 0 0 0 0";
        let sched = parse_proc_stat_scheduler(content);
        assert_eq!(sched.context_switches, 987_654_321);
        assert_eq!(sched.processes_created, 123_456);
        assert_eq!(sched.procs_running, 3);
        assert_eq!(sched.procs_blocked, 1);
    }

    #[test]
    fn test_parse_proc_stat_scheduler_empty() {
        let sched = parse_proc_stat_scheduler("");
        assert_eq!(sched.context_switches, 0);
        assert_eq!(sched.processes_created, 0);
    }

    // -- parse_proc_diskstats tests -----------------------------------------

    const SAMPLE_DISKSTATS: &str = "\
   7       0 loop0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
   1       0 ram0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
   8       0 sda 12345 100 234567 5000 67890 200 456789 8000 5 0 0 0 0 0 0 0 0
 259       0 nvme0n1 99999 50 888888 3000 55555 100 777777 4000 2 0 0 0 0 0 0 0 0";

    #[test]
    fn test_parse_proc_diskstats() {
        let disks = parse_proc_diskstats(SAMPLE_DISKSTATS);
        assert_eq!(disks.len(), 2, "loop and ram devices should be skipped");

        assert_eq!(disks[0].device, "sda");
        assert_eq!(disks[0].reads_completed, 12345);
        assert_eq!(disks[0].sectors_read, 234567);
        assert_eq!(disks[0].read_time_ms, 5000);
        assert_eq!(disks[0].writes_completed, 67890);
        assert_eq!(disks[0].sectors_written, 456789);
        assert_eq!(disks[0].write_time_ms, 8000);
        assert_eq!(disks[0].ios_in_progress, 5);

        assert_eq!(disks[1].device, "nvme0n1");
        assert_eq!(disks[1].reads_completed, 99999);
    }

    #[test]
    fn test_parse_proc_diskstats_empty() {
        let disks = parse_proc_diskstats("");
        assert!(disks.is_empty());
    }

    // -- parse_proc_vmstat tests --------------------------------------------

    const SAMPLE_VMSTAT: &str = "\
nr_free_pages 500000
nr_zone_active_anon 100000
pgfault 12345678
pgmajfault 1234
pgalloc_normal 5678900
pgfree 5500000
pgpgin 100000
pgpgout 200000
pswpin 50
pswpout 100
numa_local 999999
numa_foreign 1111
nr_dirty 500";

    #[test]
    fn test_parse_proc_vmstat() {
        let vm = parse_proc_vmstat(SAMPLE_VMSTAT);
        assert_eq!(vm.pgfault, 12_345_678);
        assert_eq!(vm.pgmajfault, 1234);
        assert_eq!(vm.pgalloc_normal, 5_678_900);
        assert_eq!(vm.pgfree, 5_500_000);
        assert_eq!(vm.pgpgin, 100_000);
        assert_eq!(vm.pgpgout, 200_000);
        assert_eq!(vm.pswpin, 50);
        assert_eq!(vm.pswpout, 100);
        assert_eq!(vm.numa_local, 999_999);
        assert_eq!(vm.numa_foreign, 1111);
    }

    #[test]
    fn test_parse_proc_vmstat_empty() {
        let vm = parse_proc_vmstat("");
        assert_eq!(vm.pgfault, 0);
        assert_eq!(vm.pgmajfault, 0);
    }

    // -- parse_proc_net_tcp tests -------------------------------------------

    const SAMPLE_NET_TCP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0CEA 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0
   1: 0100007F:1F40 0100007F:E1A8 01 00000000:00000000 00:00000000 00000000  1000        0 23456 1 0
   2: 0100007F:E1A8 0100007F:1F40 01 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0
   3: AC1100C8:01BB AC110064:C958 06 00000000:00000000 03:00000000 00000000     0        0 0 3 0
   4: AC1100C8:0050 AC110032:D4B0 08 00000000:00000000 00:00000000 00000000    33        0 45678 1 0
   5: AC1100C8:01BB AC110096:F234 01 00000000:00000000 00:00000000 00000000     0        0 56789 1 0";

    #[test]
    fn test_parse_proc_net_tcp() {
        let tcp = parse_proc_net_tcp(SAMPLE_NET_TCP);
        assert_eq!(tcp.total, 6);
        assert_eq!(tcp.established, 3); // lines with st=01
        assert_eq!(tcp.time_wait, 1); // lines with st=06
        assert_eq!(tcp.close_wait, 1); // lines with st=08
    }

    #[test]
    fn test_parse_proc_net_tcp_empty() {
        let tcp = parse_proc_net_tcp("");
        assert_eq!(tcp.total, 0);
        assert_eq!(tcp.established, 0);
    }

    #[test]
    fn test_parse_proc_net_tcp_header_only() {
        let tcp = parse_proc_net_tcp("  sl  local_address rem_address   st tx_queue rx_queue");
        assert_eq!(tcp.total, 0);
    }

    // -- Updated snapshot event test ----------------------------------------

    #[tokio::test]
    async fn test_collector_read_events_includes_new_fields() {
        let mut collector = ProcSysCollector::new();
        collector.attach().await.unwrap();

        let events = collector.read_events().await.unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&events[0].payload).expect("valid JSON");

        // Verify new fields are present in the payload
        assert!(payload.get("scheduler").is_some());
        assert!(payload.get("disk_io_devices").is_some());
        assert!(payload.get("vmstat").is_some());
        assert!(payload.get("tcp").is_some());
    }
}
