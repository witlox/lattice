//! Telemetry collection and aggregation.
//!
//! Collects metrics from GPUs, network, and storage, then aggregates
//! per the configured telemetry mode:
//! - Production: 30s bicubic aggregation
//! - Debug: 1s raw samples
//! - Audit: access logs + metrics
//!
//! Submodules:
//! - [`log_buffer`]: Per-allocation log ring buffer with S3 flush support.
//! - [`ebpf_stubs`]: Stub interfaces for eBPF-based telemetry collectors.

pub mod ebpf_stubs;
pub mod log_buffer;

pub use ebpf_stubs::{EbpfCollector, EbpfEvent, StubEbpfCollector};
pub use log_buffer::{LogRingBuffer, S3Sink};

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Telemetry resolution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryMode {
    Production,
    Debug,
    Audit,
}

impl TelemetryMode {
    /// Aggregation interval in seconds.
    pub fn interval_secs(&self) -> u64 {
        match self {
            TelemetryMode::Production => 30,
            TelemetryMode::Debug => 1,
            TelemetryMode::Audit => 10,
        }
    }
}

/// A single telemetry sample from a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySample {
    pub timestamp: DateTime<Utc>,
    pub gpu_utilization: Vec<f64>,
    pub gpu_memory_used_gb: Vec<f64>,
    pub cpu_utilization: f64,
    pub memory_used_gb: f64,
    pub network_rx_gbps: f64,
    pub network_tx_gbps: f64,
    pub storage_read_mbps: f64,
    pub storage_write_mbps: f64,
}

/// Aggregated telemetry over a time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedTelemetry {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub sample_count: u32,
    pub avg_gpu_utilization: f64,
    pub max_gpu_utilization: f64,
    pub avg_cpu_utilization: f64,
    pub avg_memory_used_gb: f64,
    pub avg_network_rx_gbps: f64,
    pub avg_network_tx_gbps: f64,
}

/// Collects and aggregates telemetry samples.
pub struct TelemetryCollector {
    mode: TelemetryMode,
    buffer: VecDeque<TelemetrySample>,
    max_buffer_size: usize,
}

impl TelemetryCollector {
    pub fn new(mode: TelemetryMode) -> Self {
        let max_buffer_size = match mode {
            TelemetryMode::Production => 120, // 30s × 120 = 1 hour at production rate
            TelemetryMode::Debug => 3600,     // 1s × 3600 = 1 hour at debug rate
            TelemetryMode::Audit => 360,      // 10s × 360 = 1 hour at audit rate
        };
        Self {
            mode,
            buffer: VecDeque::new(),
            max_buffer_size,
        }
    }

    /// Record a new sample.
    pub fn record(&mut self, sample: TelemetrySample) {
        if self.buffer.len() >= self.max_buffer_size {
            self.buffer.pop_front();
        }
        self.buffer.push_back(sample);
    }

    /// Aggregate buffered samples into a summary.
    pub fn aggregate(&self) -> Option<AggregatedTelemetry> {
        if self.buffer.is_empty() {
            return None;
        }

        let count = self.buffer.len() as f64;
        let mut total_gpu_util = 0.0;
        let mut max_gpu_util: f64 = 0.0;
        let mut total_cpu = 0.0;
        let mut total_mem = 0.0;
        let mut total_rx = 0.0;
        let mut total_tx = 0.0;

        for sample in &self.buffer {
            let avg_gpu: f64 = if sample.gpu_utilization.is_empty() {
                0.0
            } else {
                sample.gpu_utilization.iter().sum::<f64>() / sample.gpu_utilization.len() as f64
            };
            total_gpu_util += avg_gpu;
            max_gpu_util = max_gpu_util.max(avg_gpu);
            total_cpu += sample.cpu_utilization;
            total_mem += sample.memory_used_gb;
            total_rx += sample.network_rx_gbps;
            total_tx += sample.network_tx_gbps;
        }

        Some(AggregatedTelemetry {
            window_start: self.buffer.front().unwrap().timestamp,
            window_end: self.buffer.back().unwrap().timestamp,
            sample_count: self.buffer.len() as u32,
            avg_gpu_utilization: total_gpu_util / count,
            max_gpu_utilization: max_gpu_util,
            avg_cpu_utilization: total_cpu / count,
            avg_memory_used_gb: total_mem / count,
            avg_network_rx_gbps: total_rx / count,
            avg_network_tx_gbps: total_tx / count,
        })
    }

    /// Get the current telemetry mode.
    pub fn mode(&self) -> TelemetryMode {
        self.mode
    }

    /// Switch the telemetry mode (e.g., from production to debug).
    pub fn set_mode(&mut self, mode: TelemetryMode) {
        self.mode = mode;
    }

    /// Number of buffered samples.
    pub fn sample_count(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_sample(gpu_util: f64, cpu_util: f64) -> TelemetrySample {
        TelemetrySample {
            timestamp: Utc::now(),
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

    #[test]
    fn telemetry_mode_intervals() {
        assert_eq!(TelemetryMode::Production.interval_secs(), 30);
        assert_eq!(TelemetryMode::Debug.interval_secs(), 1);
        assert_eq!(TelemetryMode::Audit.interval_secs(), 10);
    }

    #[test]
    fn empty_collector_aggregates_none() {
        let collector = TelemetryCollector::new(TelemetryMode::Production);
        assert!(collector.aggregate().is_none());
    }

    #[test]
    fn single_sample_aggregation() {
        let mut collector = TelemetryCollector::new(TelemetryMode::Production);
        collector.record(make_sample(0.8, 0.5));

        let agg = collector.aggregate().unwrap();
        assert_eq!(agg.sample_count, 1);
        assert!((agg.avg_gpu_utilization - 0.8).abs() < 0.001);
        assert!((agg.avg_cpu_utilization - 0.5).abs() < 0.001);
    }

    #[test]
    fn multi_sample_averaging() {
        let mut collector = TelemetryCollector::new(TelemetryMode::Debug);
        collector.record(make_sample(0.6, 0.4));
        collector.record(make_sample(0.8, 0.6));

        let agg = collector.aggregate().unwrap();
        assert_eq!(agg.sample_count, 2);
        assert!((agg.avg_gpu_utilization - 0.7).abs() < 0.001);
        assert!((agg.max_gpu_utilization - 0.8).abs() < 0.001);
        assert!((agg.avg_cpu_utilization - 0.5).abs() < 0.001);
    }

    #[test]
    fn buffer_evicts_oldest() {
        let mut collector = TelemetryCollector::new(TelemetryMode::Production);
        // Fill buffer past max
        for i in 0..130 {
            let mut sample = make_sample(0.5, 0.5);
            sample.timestamp = Utc::now() + Duration::seconds(i);
            collector.record(sample);
        }

        // Buffer should be capped at max_buffer_size
        assert_eq!(collector.sample_count(), 120);
    }

    #[test]
    fn mode_switch() {
        let mut collector = TelemetryCollector::new(TelemetryMode::Production);
        assert_eq!(collector.mode(), TelemetryMode::Production);

        collector.set_mode(TelemetryMode::Debug);
        assert_eq!(collector.mode(), TelemetryMode::Debug);
    }

    #[test]
    fn multi_gpu_utilization_averaged() {
        let mut collector = TelemetryCollector::new(TelemetryMode::Production);
        let sample = TelemetrySample {
            timestamp: Utc::now(),
            gpu_utilization: vec![0.5, 0.7, 0.9, 0.3],
            gpu_memory_used_gb: vec![10.0, 20.0, 30.0, 5.0],
            cpu_utilization: 0.5,
            memory_used_gb: 128.0,
            network_rx_gbps: 10.0,
            network_tx_gbps: 5.0,
            storage_read_mbps: 100.0,
            storage_write_mbps: 50.0,
        };
        collector.record(sample);

        let agg = collector.aggregate().unwrap();
        // Average GPU util = (0.5 + 0.7 + 0.9 + 0.3) / 4 = 0.6
        assert!((agg.avg_gpu_utilization - 0.6).abs() < 0.001);
    }
}
