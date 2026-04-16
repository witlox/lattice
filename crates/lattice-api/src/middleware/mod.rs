//! Middleware components for the Lattice API.
//!
//! - [`cert_san`]: INV-D14 cert-SAN binding validator for agent registration.
//! - [`oidc`]: OIDC token validation trait and stub implementation.
//! - [`rate_limit`]: Per-user token-bucket rate limiter.
//! - [`rbac`]: Role-based access control policy and role derivation.

pub mod cert_san;
pub mod oidc;
pub mod rate_limit;
pub mod rbac;
