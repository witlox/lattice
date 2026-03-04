//! Conformance fingerprint computation.
//!
//! A conformance fingerprint is a SHA-256 hash of the node's software
//! stack: GPU driver version, NIC firmware, BIOS/BMC firmware, and
//! kernel parameters. Nodes with identical fingerprints are guaranteed
//! to behave identically for a given workload.
//!
//! Sensitive workloads require all nodes in an allocation to have the
//! same fingerprint. Conformance drift triggers immediate Draining.

use sha2::{Digest, Sha256};

/// Components that contribute to the conformance fingerprint.
#[derive(Debug, Clone, Default)]
pub struct ConformanceComponents {
    pub gpu_driver_version: String,
    pub nic_firmware_version: String,
    pub bios_version: String,
    pub bmc_firmware_version: String,
    pub kernel_version: String,
    pub kernel_parameters: Vec<String>,
}

/// Compute the SHA-256 conformance fingerprint from components.
pub fn compute_fingerprint(components: &ConformanceComponents) -> String {
    let mut hasher = Sha256::new();
    hasher.update(components.gpu_driver_version.as_bytes());
    hasher.update(b"|");
    hasher.update(components.nic_firmware_version.as_bytes());
    hasher.update(b"|");
    hasher.update(components.bios_version.as_bytes());
    hasher.update(b"|");
    hasher.update(components.bmc_firmware_version.as_bytes());
    hasher.update(b"|");
    hasher.update(components.kernel_version.as_bytes());
    hasher.update(b"|");
    // Sort kernel parameters for deterministic hashing
    let mut params = components.kernel_parameters.clone();
    params.sort();
    for param in &params {
        hasher.update(param.as_bytes());
        hasher.update(b",");
    }
    let result = hasher.finalize();
    hex::encode(result)
}

/// Check if a fingerprint has drifted from the expected value.
pub fn has_drifted(current: &str, expected: &str) -> bool {
    current != expected
}

// Inline hex encoding to avoid adding a dependency
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_components() -> ConformanceComponents {
        ConformanceComponents {
            gpu_driver_version: "535.129.03".to_string(),
            nic_firmware_version: "22.39.1002".to_string(),
            bios_version: "2.7.1".to_string(),
            bmc_firmware_version: "13.1".to_string(),
            kernel_version: "6.1.0-cray".to_string(),
            kernel_parameters: vec!["iommu=pt".to_string(), "hugepages=2048".to_string()],
        }
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let components = test_components();
        let fp1 = compute_fingerprint(&components);
        let fp2 = compute_fingerprint(&components);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_is_hex_string() {
        let fp = compute_fingerprint(&test_components());
        assert_eq!(fp.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn different_driver_changes_fingerprint() {
        let mut c1 = test_components();
        let mut c2 = test_components();
        c2.gpu_driver_version = "535.130.00".to_string();

        assert_ne!(compute_fingerprint(&c1), compute_fingerprint(&c2));

        // But identical components give same fingerprint
        c1.gpu_driver_version = c2.gpu_driver_version.clone();
        assert_eq!(compute_fingerprint(&c1), compute_fingerprint(&c2));
    }

    #[test]
    fn kernel_parameter_order_does_not_matter() {
        let mut c1 = test_components();
        c1.kernel_parameters = vec!["a=1".into(), "b=2".into()];

        let mut c2 = test_components();
        c2.kernel_parameters = vec!["b=2".into(), "a=1".into()];

        assert_eq!(compute_fingerprint(&c1), compute_fingerprint(&c2));
    }

    #[test]
    fn drift_detected() {
        assert!(has_drifted("abc123", "def456"));
    }

    #[test]
    fn no_drift_when_same() {
        assert!(!has_drifted("abc123", "abc123"));
    }

    #[test]
    fn empty_components_produce_valid_fingerprint() {
        let components = ConformanceComponents::default();
        let fp = compute_fingerprint(&components);
        assert_eq!(fp.len(), 64);
    }
}
