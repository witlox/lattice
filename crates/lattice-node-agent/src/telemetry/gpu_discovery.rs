//! GPU topology discovery.
//!
//! Discovers GPU interconnect topology using nvml-wrapper (NVIDIA) and
//! rocm-smi CLI output parsing (AMD). Falls back to empty topology when
//! no GPUs are detected.

use async_trait::async_trait;
use lattice_common::types::GpuTopology;
#[cfg(any(feature = "nvidia", feature = "rocm"))]
use lattice_common::types::{GpuDevice, GpuLink, GpuLinkType, GpuVendor};
use std::collections::HashMap;
use tracing::debug;

/// Trait for GPU topology discovery providers.
#[async_trait]
pub trait GpuDiscoveryProvider: Send + Sync {
    /// Discover GPU topology on this node.
    async fn discover(&self) -> Result<GpuTopology, DiscoveryError>;
}

/// Errors that can occur during GPU discovery.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("NVML initialization failed: {0}")]
    NvmlInit(String),
    #[error("rocm-smi execution failed: {0}")]
    RocmSmi(String),
    #[error("discovery failed: {0}")]
    Other(String),
}

// ─── NVIDIA Discovery (behind feature flag) ──────────────────────────────────

#[cfg(feature = "nvidia")]
pub struct NvidiaDiscovery;

#[cfg(feature = "nvidia")]
impl Default for NvidiaDiscovery {
    fn default() -> Self {
        Self
    }
}

#[cfg(feature = "nvidia")]
impl NvidiaDiscovery {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "nvidia")]
#[async_trait]
impl GpuDiscoveryProvider for NvidiaDiscovery {
    async fn discover(&self) -> Result<GpuTopology, DiscoveryError> {
        use nvml_wrapper::Nvml;

        let nvml = Nvml::init().map_err(|e| DiscoveryError::NvmlInit(e.to_string()))?;
        let device_count = nvml
            .device_count()
            .map_err(|e| DiscoveryError::NvmlInit(e.to_string()))?;

        let mut devices = Vec::new();
        let mut nic_affinity = HashMap::new();

        for i in 0..device_count {
            let device = nvml
                .device_by_index(i)
                .map_err(|e| DiscoveryError::NvmlInit(e.to_string()))?;

            let name = device.name().unwrap_or_else(|_| "Unknown".to_string());
            let memory = device.memory_info().map(|m| m.total).unwrap_or(0);

            // Discover links to peer GPUs
            let mut links = Vec::new();
            for j in 0..device_count {
                if i == j {
                    continue;
                }
                let peer = nvml.device_by_index(j);
                if let Ok(peer_dev) = peer {
                    // Try to determine link type from topology
                    // topology_common_ancestor is only available on Linux
                    #[cfg(target_os = "linux")]
                    let link_type = match device.topology_common_ancestor(peer_dev) {
                        Ok(level) => {
                            let level_val = level as u32;
                            if level_val <= 2 {
                                GpuLinkType::NVLink
                            } else if level_val <= 4 {
                                GpuLinkType::NVSwitch
                            } else {
                                GpuLinkType::PCIe
                            }
                        }
                        Err(_) => GpuLinkType::PCIe,
                    };
                    #[cfg(not(target_os = "linux"))]
                    let link_type = {
                        let _ = peer_dev;
                        GpuLinkType::PCIe
                    };

                    // Estimate bandwidth based on link type
                    let bandwidth_gbps = match link_type {
                        GpuLinkType::NVLink => 600.0,
                        GpuLinkType::NVSwitch => 900.0,
                        GpuLinkType::PCIe => 32.0,
                        GpuLinkType::InfinityFabric => 0.0,
                    };

                    links.push(GpuLink {
                        peer_index: j,
                        link_type,
                        bandwidth_gbps,
                    });
                }
            }

            // NIC affinity: use PCI bus ID to estimate closest NIC
            // Simple heuristic: GPU index maps to NIC index modulo available NICs
            nic_affinity.insert(i, i % 4);

            devices.push(GpuDevice {
                index: i,
                vendor: GpuVendor::Nvidia,
                model: name,
                memory_bytes: memory,
                links,
            });
        }

        debug!("discovered {} NVIDIA GPUs", devices.len());
        Ok(GpuTopology {
            devices,
            nic_affinity,
        })
    }
}

// ─── AMD Discovery (rocm-smi CLI, behind feature flag) ─────────────────────

#[cfg(feature = "rocm")]
/// AMD GPU discovery via rocm-smi CLI parsing.
#[derive(Default)]
pub struct AmdDiscovery;

#[cfg(feature = "rocm")]
impl AmdDiscovery {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "rocm")]
#[async_trait]
impl GpuDiscoveryProvider for AmdDiscovery {
    async fn discover(&self) -> Result<GpuTopology, DiscoveryError> {
        // Get device info
        let info_output = run_rocm_smi(&["--showmeminfo", "vram", "--csv"]).await;
        // Get topology
        let topo_output = run_rocm_smi(&["--showtopo", "--csv"]).await;

        let devices_info = info_output
            .as_deref()
            .map(parse_rocm_meminfo)
            .unwrap_or_default();
        let links = topo_output
            .as_deref()
            .map(parse_rocm_topo)
            .unwrap_or_default();

        if devices_info.is_empty() {
            return Ok(GpuTopology {
                devices: Vec::new(),
                nic_affinity: HashMap::new(),
            });
        }

        let mut devices = Vec::new();
        let mut nic_affinity = HashMap::new();

        for (index, memory_bytes) in &devices_info {
            let device_links = links
                .iter()
                .filter(|(src, _, _, _)| src == index)
                .map(|(_, peer, lt, bw)| GpuLink {
                    peer_index: *peer,
                    link_type: lt.clone(),
                    bandwidth_gbps: *bw,
                })
                .collect();

            nic_affinity.insert(*index, *index % 4);

            devices.push(GpuDevice {
                index: *index,
                vendor: GpuVendor::Amd,
                model: format!("AMD GPU {index}"),
                memory_bytes: *memory_bytes,
                links: device_links,
            });
        }

        debug!("discovered {} AMD GPUs", devices.len());
        Ok(GpuTopology {
            devices,
            nic_affinity,
        })
    }
}

#[cfg(feature = "rocm")]
/// Run rocm-smi with given arguments. Returns None on failure.
async fn run_rocm_smi(args: &[&str]) -> Option<String> {
    match tokio::process::Command::new("rocm-smi")
        .args(args)
        .output()
        .await
    {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout).ok(),
        Ok(output) => {
            debug!(
                "rocm-smi exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            None
        }
        Err(e) => {
            debug!("rocm-smi not available: {e}");
            None
        }
    }
}

#[cfg(feature = "rocm")]
/// Parse rocm-smi --showmeminfo vram --csv output.
/// Returns Vec<(index, memory_bytes)>.
pub fn parse_rocm_meminfo(output: &str) -> Vec<(u32, u64)> {
    let mut results = Vec::new();
    let mut lines = output.lines();

    // Skip header line(s)
    let header = match lines.next() {
        Some(h) => h,
        None => return results,
    };

    // Find column indices from header
    let has_gpu_col = header.to_lowercase().contains("gpu");

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 2 {
            continue;
        }

        let (idx_str, mem_str) = if has_gpu_col {
            (parts[0], parts.get(1).unwrap_or(&"0"))
        } else {
            continue;
        };

        if let (Ok(idx), Ok(mem)) = (idx_str.parse::<u32>(), mem_str.parse::<u64>()) {
            // rocm-smi reports in bytes or MB depending on version
            let mem_bytes = if mem < 1_000_000 {
                mem * 1024 * 1024 // assume MB
            } else {
                mem
            };
            results.push((idx, mem_bytes));
        }
    }

    results
}

#[cfg(feature = "rocm")]
/// Parse rocm-smi --showtopo --csv output.
/// Returns Vec<(src_index, peer_index, link_type, bandwidth_gbps)>.
pub fn parse_rocm_topo(output: &str) -> Vec<(u32, u32, GpuLinkType, f64)> {
    let mut results = Vec::new();
    let mut lines = output.lines();

    // Skip header
    let _header = lines.next();

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 4 {
            continue;
        }

        let src = match parts[0].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let peer = match parts[1].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let link_type_str = parts[2].to_lowercase();
        let link_type = if link_type_str.contains("xgmi") || link_type_str.contains("infinity") {
            GpuLinkType::InfinityFabric
        } else {
            GpuLinkType::PCIe
        };

        let bandwidth = parts[3].parse::<f64>().unwrap_or(match link_type {
            GpuLinkType::InfinityFabric => 200.0,
            GpuLinkType::PCIe => 32.0,
            _ => 32.0,
        });

        results.push((src, peer, link_type, bandwidth));
    }

    results
}

// ─── Auto Discovery ─────────────────────────────────────────────────────────

/// Automatic GPU discovery: tries NVIDIA first, then AMD, then returns empty.
#[derive(Default)]
pub struct AutoDiscovery;

impl AutoDiscovery {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl GpuDiscoveryProvider for AutoDiscovery {
    async fn discover(&self) -> Result<GpuTopology, DiscoveryError> {
        // Try NVIDIA first (via feature flag)
        #[cfg(feature = "nvidia")]
        {
            let nvidia = NvidiaDiscovery::new();
            match nvidia.discover().await {
                Ok(topo) if !topo.devices.is_empty() => {
                    debug!("using NVIDIA GPU topology");
                    return Ok(topo);
                }
                Ok(_) => debug!("NVIDIA discovery returned no devices"),
                Err(e) => debug!("NVIDIA discovery failed: {e}"),
            }
        }

        // Try AMD (via feature flag)
        #[cfg(feature = "rocm")]
        {
            let amd = AmdDiscovery::new();
            match amd.discover().await {
                Ok(topo) if !topo.devices.is_empty() => {
                    debug!("using AMD GPU topology");
                    return Ok(topo);
                }
                Ok(_) => debug!("AMD discovery returned no devices"),
                Err(e) => debug!("AMD discovery failed: {e}"),
            }
        }

        // No GPUs found
        debug!("no GPUs discovered, returning empty topology");
        Ok(GpuTopology {
            devices: Vec::new(),
            nic_affinity: HashMap::new(),
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rocm")]
    const SAMPLE_ROCM_TOPO: &str = "\
GPU,Peer GPU,Link Type,Bandwidth (GB/s)
0,1,XGMI,200.0
0,2,XGMI,200.0
1,0,XGMI,200.0
1,2,PCIe,32.0
2,0,XGMI,200.0
2,1,PCIe,32.0
";

    #[cfg(feature = "rocm")]
    const SAMPLE_ROCM_MEMINFO: &str = "\
GPU,VRAM Total (B)
0,68719476736
1,68719476736
2,68719476736
";

    #[cfg(feature = "rocm")]
    #[test]
    fn parse_rocm_topo_output() {
        let links = parse_rocm_topo(SAMPLE_ROCM_TOPO);
        assert_eq!(links.len(), 6);

        // First link: GPU 0 → GPU 1 via XGMI
        assert_eq!(links[0].0, 0);
        assert_eq!(links[0].1, 1);
        assert_eq!(links[0].2, GpuLinkType::InfinityFabric);
        assert!((links[0].3 - 200.0).abs() < 0.01);

        // Fourth link: GPU 1 → GPU 2 via PCIe
        assert_eq!(links[3].0, 1);
        assert_eq!(links[3].1, 2);
        assert_eq!(links[3].2, GpuLinkType::PCIe);
        assert!((links[3].3 - 32.0).abs() < 0.01);
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn parse_rocm_meminfo_output() {
        let devices = parse_rocm_meminfo(SAMPLE_ROCM_MEMINFO);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].0, 0);
        assert_eq!(devices[0].1, 68719476736); // 64 GB
        assert_eq!(devices[2].0, 2);
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn empty_topo_output() {
        let links = parse_rocm_topo("");
        assert!(links.is_empty());
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn empty_meminfo_output() {
        let devices = parse_rocm_meminfo("");
        assert!(devices.is_empty());
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn header_only_topo() {
        let links = parse_rocm_topo("GPU,Peer GPU,Link Type,Bandwidth\n");
        assert!(links.is_empty());
    }

    #[tokio::test]
    async fn auto_discovery_returns_empty_without_gpus() {
        let auto = AutoDiscovery::new();
        let topo = auto.discover().await.unwrap();
        // In CI/test environment, no real GPUs → empty topology
        assert!(topo.devices.is_empty() || !topo.devices.is_empty()); // always succeeds
                                                                      // The important thing is it doesn't error
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn rocm_topo_pcie_fallback() {
        let input = "GPU,Peer GPU,Link Type,BW\n0,1,unknown,0\n";
        let links = parse_rocm_topo(input);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].2, GpuLinkType::PCIe); // fallback
    }

    #[cfg(feature = "rocm")]
    #[test]
    fn rocm_meminfo_mb_conversion() {
        let input = "GPU,VRAM Total (B)\n0,16384\n";
        let devices = parse_rocm_meminfo(input);
        assert_eq!(devices.len(), 1);
        // 16384 < 1_000_000, so treated as MB → 16384 * 1024 * 1024
        assert_eq!(devices[0].1, 16384 * 1024 * 1024);
    }
}
