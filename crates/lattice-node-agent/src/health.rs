//! Health check aggregation — combines GPU, network, storage, and
//! conformance checks into a single health report.

use lattice_common::traits::NodeHealthReport;
use lattice_common::types::NodeCapabilities;

/// Individual health check result.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub passed: bool,
    pub issue: Option<String>,
}

/// Aggregated health status for a node.
#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub checks: Vec<HealthCheck>,
}

impl HealthStatus {
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    pub fn issues(&self) -> Vec<String> {
        self.checks.iter().filter_map(|c| c.issue.clone()).collect()
    }

    pub fn to_report(&self) -> NodeHealthReport {
        NodeHealthReport {
            healthy: self.healthy(),
            issues: self.issues(),
        }
    }
}

/// Runs health checks against expected capabilities.
pub struct HealthChecker {
    expected_capabilities: NodeCapabilities,
}

impl HealthChecker {
    pub fn new(expected_capabilities: NodeCapabilities) -> Self {
        Self {
            expected_capabilities,
        }
    }

    /// Run all health checks with observed values.
    pub fn check(&self, observed: &ObservedHealth) -> HealthStatus {
        let mut checks = Vec::new();

        // GPU count check
        checks.push(self.check_gpu_count(observed.gpu_count));

        // GPU temperature check
        if let Some(temp) = observed.max_gpu_temp_c {
            checks.push(self.check_gpu_temperature(temp));
        }

        // ECC error check
        checks.push(self.check_ecc_errors(observed.ecc_errors));

        // Network check
        checks.push(self.check_network(observed.nic_up));

        HealthStatus { checks }
    }

    fn check_gpu_count(&self, observed: u32) -> HealthCheck {
        let expected = self.expected_capabilities.gpu_count;
        if observed >= expected {
            HealthCheck {
                name: "gpu_count".to_string(),
                passed: true,
                issue: None,
            }
        } else {
            HealthCheck {
                name: "gpu_count".to_string(),
                passed: false,
                issue: Some(format!("expected {expected} GPUs, found {observed}")),
            }
        }
    }

    fn check_gpu_temperature(&self, max_temp_c: f64) -> HealthCheck {
        const CRITICAL_TEMP: f64 = 90.0;
        if max_temp_c < CRITICAL_TEMP {
            HealthCheck {
                name: "gpu_temperature".to_string(),
                passed: true,
                issue: None,
            }
        } else {
            HealthCheck {
                name: "gpu_temperature".to_string(),
                passed: false,
                issue: Some(format!(
                    "GPU temperature {max_temp_c}°C exceeds critical threshold {CRITICAL_TEMP}°C"
                )),
            }
        }
    }

    fn check_ecc_errors(&self, count: u64) -> HealthCheck {
        const ECC_THRESHOLD: u64 = 10;
        if count < ECC_THRESHOLD {
            HealthCheck {
                name: "ecc_errors".to_string(),
                passed: true,
                issue: None,
            }
        } else {
            HealthCheck {
                name: "ecc_errors".to_string(),
                passed: false,
                issue: Some(format!(
                    "ECC error count {count} exceeds threshold {ECC_THRESHOLD}"
                )),
            }
        }
    }

    fn check_network(&self, nic_up: bool) -> HealthCheck {
        if nic_up {
            HealthCheck {
                name: "network".to_string(),
                passed: true,
                issue: None,
            }
        } else {
            HealthCheck {
                name: "network".to_string(),
                passed: false,
                issue: Some("NIC is down".to_string()),
            }
        }
    }
}

/// Observed health values collected from the node.
#[derive(Debug, Clone, Default)]
pub struct ObservedHealth {
    pub gpu_count: u32,
    pub max_gpu_temp_c: Option<f64>,
    pub ecc_errors: u64,
    pub nic_up: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn healthy_node_passes_all_checks() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: true,
        };

        let status = checker.check(&observed);
        assert!(status.healthy());
        assert!(status.issues().is_empty());
    }

    #[test]
    fn missing_gpu_fails_check() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 3,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: true,
        };

        let status = checker.check(&observed);
        assert!(!status.healthy());
        assert!(status.issues()[0].contains("expected 4 GPUs, found 3"));
    }

    #[test]
    fn high_gpu_temperature_fails() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(95.0),
            ecc_errors: 0,
            nic_up: true,
        };

        let status = checker.check(&observed);
        assert!(!status.healthy());
        assert!(status.issues()[0].contains("temperature"));
    }

    #[test]
    fn ecc_errors_above_threshold_fails() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 15,
            nic_up: true,
        };

        let status = checker.check(&observed);
        assert!(!status.healthy());
        assert!(status.issues()[0].contains("ECC"));
    }

    #[test]
    fn nic_down_fails() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: Some(65.0),
            ecc_errors: 0,
            nic_up: false,
        };

        let status = checker.check(&observed);
        assert!(!status.healthy());
        assert!(status.issues()[0].contains("NIC"));
    }

    #[test]
    fn multiple_failures_reported() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 2,
            max_gpu_temp_c: Some(95.0),
            ecc_errors: 20,
            nic_up: false,
        };

        let status = checker.check(&observed);
        assert!(!status.healthy());
        assert_eq!(status.issues().len(), 4);
    }

    #[test]
    fn no_temperature_skips_check() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 4,
            max_gpu_temp_c: None,
            ecc_errors: 0,
            nic_up: true,
        };

        let status = checker.check(&observed);
        assert!(status.healthy());
        // Only 3 checks (gpu_count, ecc, network) — no temperature
        assert_eq!(status.checks.len(), 3);
    }

    #[test]
    fn health_report_conversion() {
        let checker = HealthChecker::new(test_capabilities());
        let observed = ObservedHealth {
            gpu_count: 3,
            max_gpu_temp_c: None,
            ecc_errors: 0,
            nic_up: true,
        };

        let status = checker.check(&observed);
        let report = status.to_report();
        assert!(!report.healthy);
        assert_eq!(report.issues.len(), 1);
    }
}
