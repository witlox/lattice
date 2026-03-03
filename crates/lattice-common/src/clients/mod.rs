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
#[cfg(feature = "federation")]
pub use sovra::HttpSovraClient;
pub use sovra::{SovraClient, StubSovraClient};
pub use vast::VastClient;
#[cfg(feature = "accounting")]
pub use waldur::HttpWaldurClient;
pub use waldur::{AccountingClient, InMemoryAccountingClient};
