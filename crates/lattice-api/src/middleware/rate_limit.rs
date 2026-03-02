//! Per-user token-bucket rate limiter.
//!
//! Tracks request counts per user identifier and enforces a configurable
//! maximum requests per minute with a burst allowance.  All state is held
//! in memory behind a `Mutex`; no external store is required.

use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for the per-user rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum sustained requests allowed per minute (refill rate).
    pub max_requests_per_minute: u32,
    /// Number of tokens the bucket may accumulate above the per-minute quota
    /// (burst headroom).
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests_per_minute: 60,
            burst_size: 10,
        }
    }
}

// ─── Error ───────────────────────────────────────────────────────────────────

/// Error returned when a user has exceeded their rate limit.
#[derive(Debug, Clone, PartialEq, Error)]
#[error("rate limit exceeded; retry after {retry_after_secs}s")]
pub struct RateLimitError {
    /// Number of seconds the caller should wait before retrying.
    pub retry_after_secs: u64,
}

// ─── Internal bucket state ────────────────────────────────────────────────────

#[derive(Debug)]
struct Bucket {
    /// Fractional tokens available (can exceed `capacity` during refill, but
    /// is capped at `capacity` when tokens are consumed).
    tokens: f64,
    /// Moment the bucket was last updated.
    last_refill: Instant,
}

impl Bucket {
    fn new(capacity: f64) -> Self {
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens proportional to elapsed time and return the updated count.
    fn refill(&mut self, refill_rate_per_sec: f64, capacity: f64, now: Instant) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_rate_per_sec).min(capacity);
        self.last_refill = now;
    }

    /// Attempt to consume one token.  Returns `true` on success.
    fn try_consume(&mut self) -> bool {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ─── RateLimiter ─────────────────────────────────────────────────────────────

/// Token-bucket rate limiter that tracks state per user identifier.
pub struct RateLimiter {
    config: RateLimitConfig,
    /// Per-user bucket state.
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    /// Create a new limiter from the given configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Create a limiter with default configuration.
    pub fn default_config() -> Self {
        Self::new(RateLimitConfig::default())
    }

    /// Check whether `user_id` is within their rate limit.
    ///
    /// On success the token is consumed and `Ok(())` is returned.
    /// When the bucket is empty `Err(RateLimitError)` is returned with the
    /// number of seconds until the next token becomes available.
    pub fn check(&self, user_id: &str) -> Result<(), RateLimitError> {
        let capacity = (self.config.max_requests_per_minute + self.config.burst_size) as f64;
        let refill_rate_per_sec = self.config.max_requests_per_minute as f64 / 60.0;
        let now = Instant::now();

        let mut map = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = map
            .entry(user_id.to_string())
            .or_insert_with(|| Bucket::new(capacity));

        bucket.refill(refill_rate_per_sec, capacity, now);

        if bucket.try_consume() {
            Ok(())
        } else {
            // Time until one token is available.
            let secs_until_token = (1.0 / refill_rate_per_sec).ceil() as u64;
            Err(RateLimitError {
                retry_after_secs: secs_until_token,
            })
        }
    }

    /// Return the number of tokens currently available for `user_id`, or the
    /// full capacity if no requests have been made yet.  Intended for testing.
    #[cfg(test)]
    fn available_tokens(&self, user_id: &str) -> f64 {
        let capacity = (self.config.max_requests_per_minute + self.config.burst_size) as f64;
        let refill_rate_per_sec = self.config.max_requests_per_minute as f64 / 60.0;
        let now = Instant::now();
        let mut map = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = map
            .entry(user_id.to_string())
            .or_insert_with(|| Bucket::new(capacity));
        bucket.refill(refill_rate_per_sec, capacity, now);
        bucket.tokens
    }

    /// Drain all tokens for `user_id`.  Used in tests to force an empty bucket.
    #[cfg(test)]
    fn drain(&self, user_id: &str) {
        let capacity = (self.config.max_requests_per_minute + self.config.burst_size) as f64;
        let mut map = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = map
            .entry(user_id.to_string())
            .or_insert_with(|| Bucket::new(capacity));
        bucket.tokens = 0.0;
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

// ─── Helpers for simulating elapsed time ─────────────────────────────────────

/// Simulate passage of time for a bucket by back-dating its `last_refill`.
#[cfg(test)]
fn backdate_bucket(limiter: &RateLimiter, user_id: &str, elapsed: Duration) {
    let capacity = (limiter.config.max_requests_per_minute + limiter.config.burst_size) as f64;
    let mut map = limiter.buckets.lock().expect("poisoned");
    let bucket = map
        .entry(user_id.to_string())
        .or_insert_with(|| Bucket::new(capacity));
    // Subtract elapsed from last_refill so the next refill call adds tokens.
    bucket.last_refill = bucket
        .last_refill
        .checked_sub(elapsed)
        .unwrap_or(bucket.last_refill);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn small_limiter() -> RateLimiter {
        // 6 req/min + burst of 2 → capacity = 8, refill ~0.1 tok/sec
        RateLimiter::new(RateLimitConfig {
            max_requests_per_minute: 6,
            burst_size: 2,
        })
    }

    // 1. Requests within the limit pass.
    #[test]
    fn within_limit_passes() {
        let limiter = small_limiter();
        // Capacity = 8; first 8 requests should succeed.
        for _ in 0..8 {
            assert!(limiter.check("alice").is_ok());
        }
    }

    // 2. Exceeding the limit returns RateLimitError.
    #[test]
    fn exceeds_limit_returns_error() {
        let limiter = small_limiter();
        // Drain all tokens first.
        limiter.drain("alice");
        let err = limiter.check("alice").unwrap_err();
        assert!(err.retry_after_secs > 0);
    }

    // 3. retry_after_secs is populated correctly.
    #[test]
    fn retry_after_secs_is_populated() {
        let limiter = small_limiter(); // 6 req/min → refill 0.1 tok/s → 10s/token
        limiter.drain("alice");
        let err = limiter.check("alice").unwrap_err();
        // ceil(1 / 0.1) == 10
        assert_eq!(err.retry_after_secs, 10);
    }

    // 4. Different users are tracked independently.
    #[test]
    fn different_users_tracked_independently() {
        let limiter = small_limiter();
        // Drain alice's bucket only.
        limiter.drain("alice");
        // bob still has tokens.
        assert!(limiter.check("bob").is_ok());
        // alice is out.
        assert!(limiter.check("alice").is_err());
    }

    // 5. Burst allowance: capacity exceeds max_requests_per_minute.
    #[test]
    fn burst_allowance_respected() {
        let limiter = small_limiter(); // capacity = 8
        let initial = limiter.available_tokens("burst-user");
        // Initial tokens equal burst capacity.
        assert!((initial - 8.0).abs() < 1e-6, "expected 8.0, got {initial}");
    }

    // 6. Tokens refill over time (simulate elapsed time).
    #[test]
    fn tokens_refill_after_time_passes() {
        let limiter = small_limiter(); // refill 0.1 tok/s, capacity 8
        limiter.drain("alice");
        // Back-date last_refill by 20 seconds → should add 2 tokens.
        backdate_bucket(&limiter, "alice", Duration::from_secs(20));
        // Two consecutive requests should succeed.
        assert!(limiter.check("alice").is_ok());
        assert!(limiter.check("alice").is_ok());
        // Third should fail (no more tokens).
        assert!(limiter.check("alice").is_err());
    }

    // 7. Default config values are sensible.
    #[test]
    fn default_config_values() {
        let cfg = RateLimitConfig::default();
        assert_eq!(cfg.max_requests_per_minute, 60);
        assert_eq!(cfg.burst_size, 10);
    }

    // 8. RateLimitError Display message is meaningful.
    #[test]
    fn rate_limit_error_display() {
        let err = RateLimitError {
            retry_after_secs: 5,
        };
        let msg = err.to_string();
        assert!(msg.contains("5"), "expected seconds in message: {msg}");
        assert!(msg.contains("retry"), "expected 'retry' in message: {msg}");
    }

    // 9. New user starts with a full bucket.
    #[test]
    fn new_user_starts_with_full_bucket() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_requests_per_minute: 12,
            burst_size: 3,
        }); // capacity = 15
        for _ in 0..15 {
            assert!(limiter.check("new-user").is_ok());
        }
        assert!(limiter.check("new-user").is_err());
    }

    // 10. Tokens do not exceed capacity after prolonged inactivity.
    #[test]
    fn tokens_capped_at_capacity() {
        let limiter = small_limiter(); // capacity = 8
                                       // Consume one token, then simulate a very long wait.
        limiter.check("alice").ok();
        backdate_bucket(&limiter, "alice", Duration::from_secs(3600));
        let tokens = limiter.available_tokens("alice");
        // Should be capped at 8, not 360+ (3600 * 0.1).
        assert!(
            tokens <= 8.0 + 1e-6,
            "tokens should be capped at capacity, got {tokens}"
        );
    }
}
