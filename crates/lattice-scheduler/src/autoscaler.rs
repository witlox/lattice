//! Reactive autoscaler for vCluster workloads.
//!
//! Evaluates cluster utilization and queue depth to produce scale-up or
//! scale-down decisions. Implements a cooldown window to avoid oscillation.
//!
//! See `docs/architecture/autoscaling.md` for the design.

use std::time::{Duration, Instant};

/// Configuration for the autoscaler.
#[derive(Debug, Clone)]
pub struct AutoscalerConfig {
    /// How often (in seconds) the autoscaler evaluation runs.
    pub evaluation_interval_secs: u64,
    /// Utilization fraction above which scale-up is triggered (0.0–1.0).
    pub scale_up_threshold: f64,
    /// Utilization fraction below which scale-down is triggered (0.0–1.0).
    pub scale_down_threshold: f64,
    /// Minimum seconds between consecutive scale actions (avoids oscillation).
    pub cooldown_secs: u64,
    /// Minimum number of nodes to keep in the vCluster.
    pub min_nodes: u32,
    /// Maximum number of nodes allowed in the vCluster.
    pub max_nodes: u32,
}

impl Default for AutoscalerConfig {
    fn default() -> Self {
        Self {
            evaluation_interval_secs: 60,
            scale_up_threshold: 0.8,
            scale_down_threshold: 0.2,
            cooldown_secs: 300,
            min_nodes: 1,
            max_nodes: u32::MAX,
        }
    }
}

/// The decision produced by one autoscaler evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleDecision {
    /// Add `count` nodes to the vCluster.
    ScaleUp { count: u32 },
    /// Remove `count` nodes from the vCluster.
    ScaleDown { count: u32 },
    /// No scaling action needed.
    NoChange,
}

/// Reactive autoscaler: evaluates metrics and returns scale decisions.
pub struct Autoscaler {
    config: AutoscalerConfig,
    /// Timestamp of the last scaling action (None = never scaled).
    last_scale_at: Option<Instant>,
}

impl Autoscaler {
    /// Create a new autoscaler with the given configuration.
    pub fn new(config: AutoscalerConfig) -> Self {
        Self {
            config,
            last_scale_at: None,
        }
    }

    /// Evaluate current state and return a scaling decision.
    ///
    /// # Arguments
    /// - `current_node_count` – number of nodes currently allocated
    /// - `avg_utilization` – average utilization across nodes (0.0–1.0)
    /// - `queue_depth` – number of pending allocations waiting for resources
    pub fn evaluate(
        &mut self,
        current_node_count: u32,
        avg_utilization: f64,
        queue_depth: u32,
    ) -> ScaleDecision {
        // Honour cooldown window: no action if we scaled too recently.
        if let Some(last) = self.last_scale_at {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_secs(self.config.cooldown_secs) {
                return ScaleDecision::NoChange;
            }
        }

        // Scale up when utilization is high or there are queued jobs.
        if avg_utilization >= self.config.scale_up_threshold || queue_depth > 0 {
            let new_count = (current_node_count + 1).min(self.config.max_nodes);
            if new_count > current_node_count {
                self.last_scale_at = Some(Instant::now());
                return ScaleDecision::ScaleUp {
                    count: new_count - current_node_count,
                };
            }
        }

        // Scale down when utilization is low and the queue is empty.
        if avg_utilization <= self.config.scale_down_threshold && queue_depth == 0 {
            let new_count = current_node_count
                .saturating_sub(1)
                .max(self.config.min_nodes);
            if new_count < current_node_count {
                self.last_scale_at = Some(Instant::now());
                return ScaleDecision::ScaleDown {
                    count: current_node_count - new_count,
                };
            }
        }

        ScaleDecision::NoChange
    }

    /// Return the configuration used by this autoscaler.
    pub fn config(&self) -> &AutoscalerConfig {
        &self.config
    }

    /// Return when the last scaling action occurred, if any.
    pub fn last_scale_at(&self) -> Option<Instant> {
        self.last_scale_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_autoscaler() -> Autoscaler {
        Autoscaler::new(AutoscalerConfig::default())
    }

    // ── Scale-up tests ───────────────────────────────────────

    #[test]
    fn scale_up_when_utilization_above_threshold() {
        let mut scaler = default_autoscaler();
        let decision = scaler.evaluate(4, 0.9, 0);
        assert_eq!(decision, ScaleDecision::ScaleUp { count: 1 });
    }

    #[test]
    fn scale_up_when_queue_not_empty_even_with_low_util() {
        let mut scaler = default_autoscaler();
        // Utilization is low but there are waiting jobs.
        let decision = scaler.evaluate(4, 0.1, 3);
        assert_eq!(decision, ScaleDecision::ScaleUp { count: 1 });
    }

    #[test]
    fn scale_up_respects_max_nodes() {
        let config = AutoscalerConfig {
            max_nodes: 4,
            ..Default::default()
        };
        let mut scaler = Autoscaler::new(config);
        // Already at max, scale-up must not exceed it.
        let decision = scaler.evaluate(4, 0.95, 0);
        assert_eq!(decision, ScaleDecision::NoChange);
    }

    #[test]
    fn scale_up_at_threshold_boundary() {
        let mut scaler = default_autoscaler(); // threshold = 0.8
                                               // Exactly at threshold: should trigger scale-up.
        let decision = scaler.evaluate(4, 0.8, 0);
        assert_eq!(decision, ScaleDecision::ScaleUp { count: 1 });
    }

    // ── Scale-down tests ─────────────────────────────────────

    #[test]
    fn scale_down_when_utilization_below_threshold() {
        let mut scaler = default_autoscaler();
        let decision = scaler.evaluate(4, 0.1, 0);
        assert_eq!(decision, ScaleDecision::ScaleDown { count: 1 });
    }

    #[test]
    fn scale_down_respects_min_nodes() {
        let config = AutoscalerConfig {
            min_nodes: 4,
            ..Default::default()
        };
        let mut scaler = Autoscaler::new(config);
        // Already at min, scale-down must not go below it.
        let decision = scaler.evaluate(4, 0.05, 0);
        assert_eq!(decision, ScaleDecision::NoChange);
    }

    #[test]
    fn no_scale_down_when_queue_has_work() {
        let mut scaler = default_autoscaler();
        // Low utilization but pending work: don't scale down.
        let decision = scaler.evaluate(4, 0.1, 2);
        // Queue is non-zero, so scale-up path fires instead.
        assert_eq!(decision, ScaleDecision::ScaleUp { count: 1 });
    }

    #[test]
    fn scale_down_at_threshold_boundary() {
        let mut scaler = default_autoscaler(); // scale_down_threshold = 0.2
                                               // Exactly at threshold: should trigger scale-down.
        let decision = scaler.evaluate(4, 0.2, 0);
        assert_eq!(decision, ScaleDecision::ScaleDown { count: 1 });
    }

    // ── No-change tests ──────────────────────────────────────

    #[test]
    fn no_change_when_utilization_in_band() {
        let mut scaler = default_autoscaler(); // band = (0.2, 0.8)
        let decision = scaler.evaluate(4, 0.5, 0);
        assert_eq!(decision, ScaleDecision::NoChange);
    }

    #[test]
    fn no_change_just_below_up_threshold() {
        let mut scaler = default_autoscaler();
        // Just below scale-up threshold, no queue.
        let decision = scaler.evaluate(4, 0.79, 0);
        assert_eq!(decision, ScaleDecision::NoChange);
    }

    #[test]
    fn no_change_just_above_down_threshold() {
        let mut scaler = default_autoscaler();
        // Just above scale-down threshold, no queue.
        let decision = scaler.evaluate(4, 0.21, 0);
        assert_eq!(decision, ScaleDecision::NoChange);
    }

    // ── Cooldown tests ───────────────────────────────────────

    #[test]
    fn cooldown_suppresses_second_scale_action() {
        let config = AutoscalerConfig {
            cooldown_secs: 300,
            ..Default::default()
        };
        let mut scaler = Autoscaler::new(config);

        // First action: should scale up.
        let first = scaler.evaluate(4, 0.9, 0);
        assert_eq!(first, ScaleDecision::ScaleUp { count: 1 });

        // Immediately evaluate again: cooldown not elapsed → NoChange.
        let second = scaler.evaluate(4, 0.9, 0);
        assert_eq!(second, ScaleDecision::NoChange);
    }

    #[test]
    fn no_cooldown_on_first_evaluation() {
        let mut scaler = default_autoscaler();
        // No prior scaling action: cooldown should not block.
        let decision = scaler.evaluate(4, 0.95, 0);
        assert!(matches!(decision, ScaleDecision::ScaleUp { .. }));
    }

    #[test]
    fn zero_cooldown_allows_immediate_rescale() {
        let config = AutoscalerConfig {
            cooldown_secs: 0,
            ..Default::default()
        };
        let mut scaler = Autoscaler::new(config);

        let first = scaler.evaluate(4, 0.9, 0);
        assert_eq!(first, ScaleDecision::ScaleUp { count: 1 });

        // With zero cooldown, second call should also attempt scale.
        let second = scaler.evaluate(5, 0.9, 0);
        assert_eq!(second, ScaleDecision::ScaleUp { count: 1 });
    }

    // ── Min/max bound tests ──────────────────────────────────

    #[test]
    fn min_nodes_zero_still_clamps_correctly() {
        let config = AutoscalerConfig {
            min_nodes: 0,
            ..Default::default()
        };
        let mut scaler = Autoscaler::new(config);
        // One node, util low → try to scale down to 0.
        let decision = scaler.evaluate(1, 0.0, 0);
        assert_eq!(decision, ScaleDecision::ScaleDown { count: 1 });
    }

    #[test]
    fn scale_up_count_is_one_per_evaluation() {
        let mut scaler = default_autoscaler();
        let decision = scaler.evaluate(10, 0.99, 5);
        // We add one node per cycle (not a burst).
        assert_eq!(decision, ScaleDecision::ScaleUp { count: 1 });
    }
}
