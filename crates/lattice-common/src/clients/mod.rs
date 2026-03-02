//! HTTP clients for external service integrations.
//!
//! Provides concrete implementations of the external service traits
//! defined in [`crate::traits`]:
//!
//! - [`OpenChamiClient`]: Implements [`InfrastructureService`] via OpenCHAMI REST API
//! - [`VastClient`]: Implements [`StorageService`] via VAST REST API

pub mod openchamj;
pub mod sovra;
pub mod vast;
pub mod waldur;

pub use openchamj::OpenChamiClient;
pub use sovra::{SovraClient, StubSovraClient};
pub use vast::VastClient;
pub use waldur::{AccountingClient, InMemoryAccountingClient};
