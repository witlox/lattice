//! Middleware components for the Lattice API.
//!
//! - [`oidc`]: OIDC token validation trait and stub implementation.
//! - [`rate_limit`]: Per-user token-bucket rate limiter.
//! - [`rbac`]: Role-based access control policy and role derivation.

pub mod oidc;
pub mod rate_limit;
pub mod rbac;
