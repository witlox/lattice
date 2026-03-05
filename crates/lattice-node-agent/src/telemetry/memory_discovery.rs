//! Memory topology discovery.
//!
//! Discovers NUMA domains, CXL memory tiers, and unified memory architectures
//! (NVIDIA GH200, AMD MI300A). Parses sysfs on Linux, falls back to a
//! configurable stub on other platforms.

use async_trait::async_trait;
use lattice_common::types::{
    GpuTopology, MemoryDomain, MemoryDomainType, MemoryInterconnect, MemoryLinkType, MemoryTopology,
};
use tracing::debug;

use super::gpu_discovery::DiscoveryError;

/// Trait for memory topology discovery providers.
#[async_trait]
pub trait MemoryDiscoveryProvider: Send + Sync {
    /// Discover the memory topology on this node.
    async fn discover(&self) -> Result<MemoryTopology, DiscoveryError>;
}

// ─── Sysfs Memory Discovery (Linux) ─────────────────────────────────────────

/// Discovers NUMA domains from /sys/devices/system/node/ and CXL devices
/// from /sys/bus/cxl/devices/.
pub struct SysfsMemoryDiscovery;

impl SysfsMemoryDiscovery {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SysfsMemoryDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryDiscoveryProvider for SysfsMemoryDiscovery {
    async fn discover(&self) -> Result<MemoryTopology, DiscoveryError> {
        #[cfg(target_os = "linux")]
        {
            discover_sysfs().await
        }
        #[cfg(not(target_os = "linux"))]
        {
            debug!("sysfs memory discovery not available on this platform");
            Ok(single_dram_domain(0))
        }
    }
}

/// Discover NUMA topology from sysfs (Linux only).
#[cfg(target_os = "linux")]
async fn discover_sysfs() -> Result<MemoryTopology, DiscoveryError> {
    let mut domains = Vec::new();
    let mut total_bytes = 0u64;

    // Enumerate /sys/devices/system/node/node*/
    let node_dir = "/sys/devices/system/node";
    let mut entries = match tokio::fs::read_dir(node_dir).await {
        Ok(entries) => entries,
        Err(_) => {
            debug!("cannot read {node_dir}, returning single DRAM domain");
            return Ok(single_dram_domain(0));
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("node") {
            continue;
        }
        let node_id: u32 = match name.strip_prefix("node").and_then(|s| s.parse().ok()) {
            Some(id) => id,
            None => continue,
        };

        let path = entry.path();

        // Parse meminfo for capacity
        let meminfo_path = path.join("meminfo");
        let capacity_bytes = match tokio::fs::read_to_string(&meminfo_path).await {
            Ok(content) => parse_numa_meminfo(&content, node_id),
            Err(_) => 0,
        };

        // Parse cpulist for attached CPUs
        let cpulist_path = path.join("cpulist");
        let attached_cpus = match tokio::fs::read_to_string(&cpulist_path).await {
            Ok(content) => parse_cpulist(content.trim()),
            Err(_) => Vec::new(),
        };

        total_bytes += capacity_bytes;

        domains.push(MemoryDomain {
            id: node_id,
            domain_type: MemoryDomainType::Dram,
            capacity_bytes,
            numa_node: Some(node_id),
            attached_cpus,
            attached_gpus: Vec::new(), // filled by UnifiedMemoryDiscovery if needed
        });
    }

    // Check for CXL devices
    let cxl_domains = discover_cxl_domains().await;
    let cxl_interconnects: Vec<_> = cxl_domains
        .iter()
        .map(|d| {
            // Connect CXL domain to NUMA node 0 by default
            MemoryInterconnect {
                domain_a: 0,
                domain_b: d.id,
                link_type: MemoryLinkType::CxlSwitch,
                bandwidth_gbps: 64.0, // CXL 2.0 typical
                latency_ns: 200,
            }
        })
        .collect();

    for d in &cxl_domains {
        total_bytes += d.capacity_bytes;
    }
    domains.extend(cxl_domains);

    // Build NUMA interconnects between DRAM domains
    let dram_ids: Vec<u32> = domains
        .iter()
        .filter(|d| d.domain_type == MemoryDomainType::Dram)
        .map(|d| d.id)
        .collect();
    let mut interconnects = Vec::new();
    for i in 0..dram_ids.len() {
        for j in (i + 1)..dram_ids.len() {
            interconnects.push(MemoryInterconnect {
                domain_a: dram_ids[i],
                domain_b: dram_ids[j],
                link_type: MemoryLinkType::NumaLink,
                bandwidth_gbps: 50.0, // typical DDR5 inter-socket
                latency_ns: 100,
            });
        }
    }
    interconnects.extend(cxl_interconnects);

    if domains.is_empty() {
        return Ok(single_dram_domain(0));
    }

    debug!("discovered {} memory domains from sysfs", domains.len());
    Ok(MemoryTopology {
        domains,
        interconnects,
        total_capacity_bytes: total_bytes,
    })
}

/// Check for CXL devices in sysfs.
#[cfg(target_os = "linux")]
async fn discover_cxl_domains() -> Vec<MemoryDomain> {
    let cxl_dir = "/sys/bus/cxl/devices";
    let mut entries = match tokio::fs::read_dir(cxl_dir).await {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut domains = Vec::new();
    let mut cxl_id = 100; // Start CXL domain IDs at 100

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("mem") {
            continue;
        }

        // Try to read size from the device
        let size_path = entry.path().join("size");
        let capacity = match tokio::fs::read_to_string(&size_path).await {
            Ok(s) => s.trim().parse::<u64>().unwrap_or(0),
            Err(_) => 0,
        };

        if capacity > 0 {
            domains.push(MemoryDomain {
                id: cxl_id,
                domain_type: MemoryDomainType::CxlAttached,
                capacity_bytes: capacity,
                numa_node: None,
                attached_cpus: Vec::new(),
                attached_gpus: Vec::new(),
            });
            cxl_id += 1;
        }
    }

    if !domains.is_empty() {
        debug!("discovered {} CXL memory devices", domains.len());
    }
    domains
}

/// Parse per-NUMA meminfo from /sys/devices/system/node/nodeN/meminfo.
/// Looks for "Node N MemTotal:" line.
#[cfg(target_os = "linux")]
fn parse_numa_meminfo(content: &str, node_id: u32) -> u64 {
    let prefix = format!("Node {node_id} MemTotal:");
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with(&prefix) {
            // Format: "Node 0 MemTotal:    131072 kB"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let Ok(kb) = parts[3].parse::<u64>() {
                    return kb * 1024; // kB → bytes
                }
            }
        }
    }
    0
}

// ─── Unified Memory Discovery ───────────────────────────────────────────────

/// Extends sysfs discovery with unified memory detection for superchip architectures.
///
/// Cross-references GPU model strings to detect:
/// - NVIDIA GH200: Grace-Hopper with coherent NVLink-C2C (900 Gbps)
/// - AMD MI300A: APU with coherent Infinity Fabric (896 Gbps)
pub struct UnifiedMemoryDiscovery {
    sysfs: SysfsMemoryDiscovery,
}

impl UnifiedMemoryDiscovery {
    pub fn new() -> Self {
        Self {
            sysfs: SysfsMemoryDiscovery::new(),
        }
    }

    /// Discover memory topology, merging NUMA domains for unified memory superchips.
    pub async fn discover_with_gpu(
        &self,
        gpu_topology: Option<&GpuTopology>,
    ) -> Result<MemoryTopology, DiscoveryError> {
        let mut topo = self.sysfs.discover().await?;

        if let Some(gpu_topo) = gpu_topology {
            if let Some(superchip) = detect_superchip(gpu_topo) {
                merge_unified_domains(&mut topo, gpu_topo, &superchip);
            }
        }

        Ok(topo)
    }
}

impl Default for UnifiedMemoryDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryDiscoveryProvider for UnifiedMemoryDiscovery {
    async fn discover(&self) -> Result<MemoryTopology, DiscoveryError> {
        // Without GPU info, falls back to sysfs-only discovery
        self.sysfs.discover().await
    }
}

/// Detected superchip type with bandwidth characteristics.
#[derive(Debug, Clone)]
struct SuperchipInfo {
    name: &'static str,
    bandwidth_gbps: f64,
}

/// Detect superchip architecture from GPU model strings.
fn detect_superchip(gpu_topo: &GpuTopology) -> Option<SuperchipInfo> {
    for device in &gpu_topo.devices {
        let model_upper = device.model.to_uppercase();
        if model_upper.contains("GH200") {
            return Some(SuperchipInfo {
                name: "GH200",
                bandwidth_gbps: 900.0,
            });
        }
        if model_upper.contains("MI300A") {
            return Some(SuperchipInfo {
                name: "MI300A",
                bandwidth_gbps: 896.0,
            });
        }
    }
    None
}

/// Merge CPU+GPU NUMA domains into a unified domain for superchip architectures.
fn merge_unified_domains(
    topo: &mut MemoryTopology,
    gpu_topo: &GpuTopology,
    superchip: &SuperchipInfo,
) {
    debug!(
        "detected {} superchip, merging into unified memory domain",
        superchip.name
    );

    // Collect all CPU and GPU IDs from existing domains
    let all_cpus: Vec<u32> = topo
        .domains
        .iter()
        .flat_map(|d| d.attached_cpus.iter().copied())
        .collect();
    let all_gpus: Vec<u32> = gpu_topo.devices.iter().map(|d| d.index).collect();
    let total_capacity: u64 = topo.domains.iter().map(|d| d.capacity_bytes).sum();

    // Replace all domains with a single unified domain
    topo.domains = vec![MemoryDomain {
        id: 0,
        domain_type: MemoryDomainType::Unified,
        capacity_bytes: total_capacity,
        numa_node: Some(0),
        attached_cpus: all_cpus,
        attached_gpus: all_gpus,
    }];

    // Replace interconnects with a single coherent fabric link
    topo.interconnects = vec![MemoryInterconnect {
        domain_a: 0,
        domain_b: 0, // self-link representing the coherent fabric
        link_type: MemoryLinkType::CoherentFabric,
        bandwidth_gbps: superchip.bandwidth_gbps,
        latency_ns: 50, // very low latency for on-chip coherence
    }];

    topo.total_capacity_bytes = total_capacity;
}

// ─── Stub Discovery (test / non-Linux fallback) ─────────────────────────────

/// Stub memory discovery that returns a configurable single-domain topology.
pub struct StubMemoryDiscovery {
    capacity_bytes: u64,
    domain_type: MemoryDomainType,
}

impl StubMemoryDiscovery {
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            capacity_bytes,
            domain_type: MemoryDomainType::Dram,
        }
    }

    pub fn with_type(mut self, domain_type: MemoryDomainType) -> Self {
        self.domain_type = domain_type;
        self
    }
}

impl Default for StubMemoryDiscovery {
    fn default() -> Self {
        Self::new(512 * 1024 * 1024 * 1024) // 512 GB
    }
}

#[async_trait]
impl MemoryDiscoveryProvider for StubMemoryDiscovery {
    async fn discover(&self) -> Result<MemoryTopology, DiscoveryError> {
        Ok(MemoryTopology {
            domains: vec![MemoryDomain {
                id: 0,
                domain_type: self.domain_type,
                capacity_bytes: self.capacity_bytes,
                numa_node: Some(0),
                attached_cpus: vec![0, 1, 2, 3],
                attached_gpus: vec![],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: self.capacity_bytes,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Create a single DRAM domain with zero capacity (fallback).
fn single_dram_domain(capacity_bytes: u64) -> MemoryTopology {
    MemoryTopology {
        domains: vec![MemoryDomain {
            id: 0,
            domain_type: MemoryDomainType::Dram,
            capacity_bytes,
            numa_node: Some(0),
            attached_cpus: Vec::new(),
            attached_gpus: Vec::new(),
        }],
        interconnects: Vec::new(),
        total_capacity_bytes: capacity_bytes,
    }
}

/// Parse a CPU list string like "0-3,8-11" into a vector of CPU IDs.
pub fn parse_cpulist(s: &str) -> Vec<u32> {
    let mut result = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.parse::<u32>(), end.parse::<u32>()) {
                result.extend(s..=e);
            }
        } else if let Ok(cpu) = part.parse::<u32>() {
            result.push(cpu);
        }
    }
    result
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::types::{GpuDevice, GpuVendor};
    use std::collections::HashMap;

    #[test]
    fn parse_cpulist_simple_range() {
        assert_eq!(parse_cpulist("0-3"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn parse_cpulist_comma_separated() {
        assert_eq!(parse_cpulist("0,2,4"), vec![0, 2, 4]);
    }

    #[test]
    fn parse_cpulist_mixed() {
        assert_eq!(parse_cpulist("0-3,8-11"), vec![0, 1, 2, 3, 8, 9, 10, 11]);
    }

    #[test]
    fn parse_cpulist_single() {
        assert_eq!(parse_cpulist("5"), vec![5]);
    }

    #[test]
    fn parse_cpulist_empty() {
        assert_eq!(parse_cpulist(""), Vec::<u32>::new());
    }

    #[test]
    fn detect_gh200_superchip() {
        let gpu_topo = GpuTopology {
            devices: vec![GpuDevice {
                index: 0,
                vendor: GpuVendor::Nvidia,
                model: "NVIDIA GH200 Grace Hopper".to_string(),
                memory_bytes: 96 * 1024 * 1024 * 1024,
                links: Vec::new(),
            }],
            nic_affinity: HashMap::new(),
        };
        let sc = detect_superchip(&gpu_topo).unwrap();
        assert_eq!(sc.name, "GH200");
        assert!((sc.bandwidth_gbps - 900.0).abs() < 0.01);
    }

    #[test]
    fn detect_mi300a_superchip() {
        let gpu_topo = GpuTopology {
            devices: vec![GpuDevice {
                index: 0,
                vendor: GpuVendor::Amd,
                model: "AMD Instinct MI300A".to_string(),
                memory_bytes: 128 * 1024 * 1024 * 1024,
                links: Vec::new(),
            }],
            nic_affinity: HashMap::new(),
        };
        let sc = detect_superchip(&gpu_topo).unwrap();
        assert_eq!(sc.name, "MI300A");
        assert!((sc.bandwidth_gbps - 896.0).abs() < 0.01);
    }

    #[test]
    fn detect_no_superchip_for_regular_gpu() {
        let gpu_topo = GpuTopology {
            devices: vec![GpuDevice {
                index: 0,
                vendor: GpuVendor::Nvidia,
                model: "NVIDIA A100".to_string(),
                memory_bytes: 80 * 1024 * 1024 * 1024,
                links: Vec::new(),
            }],
            nic_affinity: HashMap::new(),
        };
        assert!(detect_superchip(&gpu_topo).is_none());
    }

    #[test]
    fn merge_unified_domains_gh200() {
        let mut topo = MemoryTopology {
            domains: vec![
                MemoryDomain {
                    id: 0,
                    domain_type: MemoryDomainType::Dram,
                    capacity_bytes: 128 * 1024 * 1024 * 1024,
                    numa_node: Some(0),
                    attached_cpus: vec![0, 1, 2, 3],
                    attached_gpus: vec![],
                },
                MemoryDomain {
                    id: 1,
                    domain_type: MemoryDomainType::Dram,
                    capacity_bytes: 96 * 1024 * 1024 * 1024,
                    numa_node: Some(1),
                    attached_cpus: vec![],
                    attached_gpus: vec![],
                },
            ],
            interconnects: vec![MemoryInterconnect {
                domain_a: 0,
                domain_b: 1,
                link_type: MemoryLinkType::NumaLink,
                bandwidth_gbps: 50.0,
                latency_ns: 100,
            }],
            total_capacity_bytes: 224 * 1024 * 1024 * 1024,
        };

        let gpu_topo = GpuTopology {
            devices: vec![GpuDevice {
                index: 0,
                vendor: GpuVendor::Nvidia,
                model: "NVIDIA GH200".to_string(),
                memory_bytes: 96 * 1024 * 1024 * 1024,
                links: Vec::new(),
            }],
            nic_affinity: HashMap::new(),
        };

        let sc = SuperchipInfo {
            name: "GH200",
            bandwidth_gbps: 900.0,
        };
        merge_unified_domains(&mut topo, &gpu_topo, &sc);

        assert_eq!(topo.domains.len(), 1);
        assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Unified);
        assert_eq!(topo.domains[0].capacity_bytes, 224 * 1024 * 1024 * 1024);
        assert_eq!(topo.domains[0].attached_cpus, vec![0, 1, 2, 3]);
        assert_eq!(topo.domains[0].attached_gpus, vec![0]);
        assert_eq!(topo.interconnects.len(), 1);
        assert_eq!(
            topo.interconnects[0].link_type,
            MemoryLinkType::CoherentFabric
        );
        assert!((topo.interconnects[0].bandwidth_gbps - 900.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn stub_discovery_returns_configured_capacity() {
        let cap = 256 * 1024 * 1024 * 1024;
        let stub = StubMemoryDiscovery::new(cap);
        let topo = stub.discover().await.unwrap();
        assert_eq!(topo.total_capacity_bytes, cap);
        assert_eq!(topo.domains.len(), 1);
        assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Dram);
    }

    #[tokio::test]
    async fn stub_discovery_with_unified_type() {
        let stub =
            StubMemoryDiscovery::new(512 * 1024 * 1024 * 1024).with_type(MemoryDomainType::Unified);
        let topo = stub.discover().await.unwrap();
        assert_eq!(topo.domains[0].domain_type, MemoryDomainType::Unified);
    }

    #[tokio::test]
    async fn sysfs_discovery_does_not_panic() {
        // On any platform, this should return something (fallback on non-Linux)
        let sysfs = SysfsMemoryDiscovery::new();
        let topo = sysfs.discover().await.unwrap();
        assert!(!topo.domains.is_empty());
    }

    #[tokio::test]
    async fn unified_discovery_without_gpu_falls_back() {
        let unified = UnifiedMemoryDiscovery::new();
        let topo = unified.discover_with_gpu(None).await.unwrap();
        assert!(!topo.domains.is_empty());
        // Without GPU info, should not produce unified domains
        // (unless we happen to be on actual superchip hardware)
    }

    #[tokio::test]
    async fn unified_discovery_with_gh200_merges() {
        let unified = UnifiedMemoryDiscovery::new();
        let gpu_topo = GpuTopology {
            devices: vec![GpuDevice {
                index: 0,
                vendor: GpuVendor::Nvidia,
                model: "NVIDIA GH200 Grace Hopper".to_string(),
                memory_bytes: 96 * 1024 * 1024 * 1024,
                links: Vec::new(),
            }],
            nic_affinity: HashMap::new(),
        };
        let topo = unified.discover_with_gpu(Some(&gpu_topo)).await.unwrap();
        // Should have merged into a unified domain
        assert!(topo
            .domains
            .iter()
            .any(|d| d.domain_type == MemoryDomainType::Unified));
    }
}
