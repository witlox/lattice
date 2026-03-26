use std::fmt;

/// Errors during secret resolution. All variants are fatal startup errors.
///
/// No variant contains a resolved secret value (INV-SEC2).
/// Error messages reference paths and field names only.
#[derive(Debug)]
pub enum SecretResolutionError {
    /// Vault TCP/TLS connection or certificate verification failed.
    ConnectionFailed { address: String, source: String },
    /// Vault AppRole authentication rejected.
    AuthenticationFailed { address: String, detail: String },
    /// Vault KV v2 path does not exist.
    PathNotFound { path: String },
    /// Vault path exists but the requested key is missing.
    KeyNotFound { path: String, key: String },
    /// Non-Vault: env var unset AND config literal empty.
    NotFound { section: String, field: String },
    /// Base64 decode or size validation failed for binary secret.
    InvalidFormat {
        section: String,
        field: String,
        reason: String,
    },
    /// Unexpected Vault HTTP response.
    /// Body is truncated to 256 bytes to prevent accidental secret leakage (SEC-F7).
    VaultError {
        path: String,
        status: u16,
        body: String,
    },
    /// Multiple resolution errors collected (SEC-F8).
    Multiple(Vec<SecretResolutionError>),
    /// Environment variable for Vault secret ID is not set.
    SecretIdEnvMissing { env_var: String },
}

impl fmt::Display for SecretResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionFailed { address, source } => {
                write!(f, "Vault connection failed to {address}: {source}")
            }
            Self::AuthenticationFailed { address, detail } => {
                write!(f, "Vault authentication failed at {address}: {detail}")
            }
            Self::PathNotFound { path } => {
                write!(f, "Vault path not found: {path}")
            }
            Self::KeyNotFound { path, key } => {
                write!(f, "Vault key '{key}' not found at path {path}")
            }
            Self::NotFound { section, field } => {
                write!(
                    f,
                    "Secret {section}.{field} not found: env var LATTICE_{}_{} not set and config value is empty",
                    section.to_uppercase(),
                    field.to_uppercase()
                )
            }
            Self::InvalidFormat {
                section,
                field,
                reason,
            } => {
                write!(f, "Secret {section}.{field} has invalid format: {reason}")
            }
            Self::VaultError { path, status, body } => {
                write!(
                    f,
                    "Vault error at {path}: HTTP {status}: {}",
                    &body[..body.len().min(256)]
                )
            }
            Self::Multiple(errors) => {
                writeln!(f, "Multiple secret resolution errors:")?;
                for (i, e) in errors.iter().enumerate() {
                    writeln!(f, "  {}: {e}", i + 1)?;
                }
                Ok(())
            }
            Self::SecretIdEnvMissing { env_var } => {
                write!(f, "Vault secret_id env var '{env_var}' is not set")
            }
        }
    }
}

impl std::error::Error for SecretResolutionError {}
