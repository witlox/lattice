//! Middleware components for the Lattice API.
//!
//! - [`oidc`]: OIDC token validation trait and stub implementation.
//! - [`rate_limit`]: Per-user token-bucket rate limiter.

pub mod oidc;
pub mod rate_limit;
