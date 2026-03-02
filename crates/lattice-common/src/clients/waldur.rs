//! Waldur accounting client — external billing and resource accounting.
//!
//! Waldur tracks resource consumption per tenant for billing purposes.
//! Events are buffered and flushed asynchronously to avoid blocking
//! the scheduler hot path.
//!
//! Feature-gated behind `accounting`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{AllocId, TenantId};

/// A resource consumption event to report to Waldur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingEvent {
    /// Allocation that consumed resources.
    pub allocation_id: AllocId,
    /// Tenant being billed.
    pub tenant_id: TenantId,
    /// Resource type (e.g., "gpu_hours", "node_hours", "storage_gb_hours").
    pub resource_type: String,
    /// Amount consumed.
    pub amount: f64,
    /// Start of the consumption period.
    pub period_start: DateTime<Utc>,
    /// End of the consumption period.
    pub period_end: DateTime<Utc>,
}

/// Configuration for the Waldur client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaldurConfig {
    /// Waldur API endpoint.
    pub api_url: String,
    /// API token for authentication.
    pub api_token: String,
    /// Buffer flush interval in seconds.
    pub flush_interval_secs: u64,
    /// Maximum events to buffer before forcing a flush.
    pub max_buffer_size: usize,
}

impl Default for WaldurConfig {
    fn default() -> Self {
        Self {
            api_url: "https://waldur.example.com/api".to_string(),
            api_token: String::new(),
            flush_interval_secs: 60,
            max_buffer_size: 1000,
        }
    }
}

/// Trait for accounting event submission.
#[async_trait]
pub trait AccountingClient: Send + Sync {
    /// Submit a single accounting event.
    async fn submit_event(&self, event: AccountingEvent) -> Result<(), String>;

    /// Flush buffered events to the remote service.
    async fn flush(&self) -> Result<usize, String>;

    /// Query total consumption for a tenant within a time range.
    async fn query_usage(
        &self,
        tenant_id: &str,
        resource_type: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<f64, String>;
}

/// In-memory accounting client for testing.
pub struct InMemoryAccountingClient {
    events: std::sync::Arc<std::sync::Mutex<Vec<AccountingEvent>>>,
}

impl InMemoryAccountingClient {
    pub fn new() -> Self {
        Self {
            events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Get all recorded events.
    pub async fn events(&self) -> Vec<AccountingEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl Default for InMemoryAccountingClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AccountingClient for InMemoryAccountingClient {
    async fn submit_event(&self, event: AccountingEvent) -> Result<(), String> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    async fn flush(&self) -> Result<usize, String> {
        let events = self.events.lock().unwrap();
        Ok(events.len())
    }

    async fn query_usage(
        &self,
        tenant_id: &str,
        resource_type: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<f64, String> {
        let events = self.events.lock().unwrap();
        let total: f64 = events
            .iter()
            .filter(|e| {
                e.tenant_id == tenant_id
                    && e.resource_type == resource_type
                    && e.period_start >= from
                    && e.period_end <= to
            })
            .map(|e| e.amount)
            .sum();
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn sample_event(tenant: &str, resource: &str, amount: f64) -> AccountingEvent {
        let now = Utc::now();
        AccountingEvent {
            allocation_id: uuid::Uuid::new_v4(),
            tenant_id: tenant.to_string(),
            resource_type: resource.to_string(),
            amount,
            period_start: now - Duration::hours(1),
            period_end: now,
        }
    }

    #[tokio::test]
    async fn submit_and_query() {
        let client = InMemoryAccountingClient::new();

        client
            .submit_event(sample_event("physics", "gpu_hours", 10.0))
            .await
            .unwrap();
        client
            .submit_event(sample_event("physics", "gpu_hours", 5.0))
            .await
            .unwrap();
        client
            .submit_event(sample_event("biology", "gpu_hours", 3.0))
            .await
            .unwrap();

        let total = client
            .query_usage(
                "physics",
                "gpu_hours",
                Utc::now() - Duration::hours(2),
                Utc::now() + Duration::hours(1),
            )
            .await
            .unwrap();

        assert!((total - 15.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn flush_returns_count() {
        let client = InMemoryAccountingClient::new();
        client
            .submit_event(sample_event("t1", "node_hours", 1.0))
            .await
            .unwrap();
        client
            .submit_event(sample_event("t1", "node_hours", 2.0))
            .await
            .unwrap();

        let count = client.flush().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn query_empty_returns_zero() {
        let client = InMemoryAccountingClient::new();
        let total = client
            .query_usage(
                "nobody",
                "gpu_hours",
                Utc::now() - Duration::hours(1),
                Utc::now(),
            )
            .await
            .unwrap();
        assert!((total).abs() < 0.001);
    }

    #[tokio::test]
    async fn query_filters_by_resource_type() {
        let client = InMemoryAccountingClient::new();
        client
            .submit_event(sample_event("t1", "gpu_hours", 10.0))
            .await
            .unwrap();
        client
            .submit_event(sample_event("t1", "storage_gb_hours", 50.0))
            .await
            .unwrap();

        let gpu = client
            .query_usage(
                "t1",
                "gpu_hours",
                Utc::now() - Duration::hours(2),
                Utc::now() + Duration::hours(1),
            )
            .await
            .unwrap();
        assert!((gpu - 10.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn events_accessor() {
        let client = InMemoryAccountingClient::new();
        client
            .submit_event(sample_event("t1", "gpu_hours", 1.0))
            .await
            .unwrap();

        let events = client.events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tenant_id, "t1");
    }

    #[test]
    fn default_config() {
        let config = WaldurConfig::default();
        assert!(config.api_url.contains("waldur"));
        assert_eq!(config.flush_interval_secs, 60);
        assert_eq!(config.max_buffer_size, 1000);
    }
}
