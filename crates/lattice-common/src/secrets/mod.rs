//! Secret resolution subsystem.
//!
//! Resolves operational secrets (API credentials, signing keys) at component
//! startup. Three backends: Vault KV v2, environment variables, config literals.
//!
//! When Vault is globally configured (`vault.address` set), ALL secret fields
//! are resolved from Vault — config and env values are ignored (INV-SEC4).
//!
//! TLS certificates are NOT handled here — they use the hpc-identity cascade.

mod backends;
mod errors;
mod resolver;
mod value;
#[cfg(test)]
mod vault_tests;

pub use errors::SecretResolutionError;
pub use resolver::{ResolvedSecrets, SecretResolver};
pub use value::SecretValue;
