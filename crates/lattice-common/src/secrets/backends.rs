use std::collections::HashMap;

use super::errors::SecretResolutionError;
use super::value::SecretValue;
use crate::config::{LatticeConfig, VaultConfig};

/// Pluggable backend for fetching secret values.
pub(crate) trait SecretBackend {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError>;
}

// ─── VaultBackend ──────────────────────────────────────────

/// Fetches secrets from HashiCorp Vault KV v2 via AppRole auth.
pub(crate) struct VaultBackend {
    http: reqwest::blocking::Client,
    base_url: String,
    prefix: String,
    token: String,
}

/// Vault KV v2 response shape.
#[derive(serde::Deserialize)]
struct VaultKvResponse {
    data: VaultKvData,
}

#[derive(serde::Deserialize)]
struct VaultKvData {
    data: HashMap<String, String>,
}

/// Vault AppRole login response.
#[derive(serde::Deserialize)]
struct VaultAuthResponse {
    auth: VaultAuthData,
}

#[derive(serde::Deserialize)]
struct VaultAuthData {
    client_token: String,
}

impl VaultBackend {
    /// Construct and authenticate. Fails immediately if Vault is unreachable
    /// or auth is rejected (INV-SEC3).
    pub(crate) fn new(config: &VaultConfig) -> Result<Self, SecretResolutionError> {
        let secret_id = std::env::var(&config.secret_id_env).map_err(|_| {
            SecretResolutionError::SecretIdEnvMissing {
                env_var: config.secret_id_env.clone(),
            }
        })?;

        let mut client_builder = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs));

        if let Some(ref ca_path) = config.tls_ca_path {
            let ca_bytes =
                std::fs::read(ca_path).map_err(|e| SecretResolutionError::ConnectionFailed {
                    address: config.address.clone(),
                    source: format!("failed to read TLS CA from {}: {e}", ca_path.display()),
                })?;
            let ca_cert = reqwest::Certificate::from_pem(&ca_bytes).map_err(|e| {
                SecretResolutionError::ConnectionFailed {
                    address: config.address.clone(),
                    source: format!("invalid TLS CA certificate: {e}"),
                }
            })?;
            client_builder = client_builder.add_root_certificate(ca_cert);
        }

        let http = client_builder
            .build()
            .map_err(|e| SecretResolutionError::ConnectionFailed {
                address: config.address.clone(),
                source: format!("HTTP client construction failed: {e}"),
            })?;

        // AppRole login
        let login_url = format!(
            "{}/v1/auth/approle/login",
            config.address.trim_end_matches('/')
        );
        let login_body = serde_json::json!({
            "role_id": config.role_id,
            "secret_id": secret_id,
        });

        let resp =
            http.post(&login_url)
                .json(&login_body)
                .send()
                .map_err(
                    |e: reqwest::Error| SecretResolutionError::ConnectionFailed {
                        address: config.address.clone(),
                        source: e.to_string(),
                    },
                )?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(SecretResolutionError::AuthenticationFailed {
                address: config.address.clone(),
                detail: format!("HTTP {status}: {}", &body[..body.len().min(256)]),
            });
        }

        let auth_resp: VaultAuthResponse =
            resp.json()
                .map_err(|e| SecretResolutionError::AuthenticationFailed {
                    address: config.address.clone(),
                    detail: format!("failed to parse auth response: {e}"),
                })?;

        tracing::info!("Secret resolution via Vault at {}", config.address);

        Ok(Self {
            http,
            base_url: config.address.trim_end_matches('/').to_string(),
            prefix: config.prefix.clone(),
            token: auth_resp.auth.client_token,
        })
    }
}

impl SecretBackend for VaultBackend {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError> {
        let path = format!("{}/{section}", self.prefix);
        let url = format!("{}/v1/{path}", self.base_url);

        let resp = self
            .http
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .map_err(
                |e: reqwest::Error| SecretResolutionError::ConnectionFailed {
                    address: self.base_url.clone(),
                    source: e.to_string(),
                },
            )?;

        let status = resp.status().as_u16();
        if status == 404 {
            return Err(SecretResolutionError::PathNotFound { path });
        }
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(SecretResolutionError::VaultError {
                path,
                status,
                body: body[..body.len().min(256)].to_string(),
            });
        }

        let kv_resp: VaultKvResponse =
            resp.json().map_err(|e| SecretResolutionError::VaultError {
                path: path.clone(),
                status: 200,
                body: format!("failed to parse KV v2 response: {e}"),
            })?;

        let value =
            kv_resp
                .data
                .data
                .get(field)
                .ok_or_else(|| SecretResolutionError::KeyNotFound {
                    path: path.clone(),
                    key: field.to_string(),
                })?;

        tracing::debug!("Resolved {section}.{field} from Vault path {path}");
        Ok(SecretValue::from(value.as_str()))
    }
}

impl Drop for VaultBackend {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.token.zeroize();
    }
}

// ─── EnvBackend ────────────────────────────────────────────

/// Reads secrets from environment variables.
/// Convention: `LATTICE_{SECTION}_{FIELD}` (uppercase).
pub(crate) struct EnvBackend;

impl SecretBackend for EnvBackend {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError> {
        let var_name = format!(
            "LATTICE_{}_{}",
            section.to_uppercase(),
            field.to_uppercase()
        );
        match std::env::var(&var_name) {
            Ok(val) if !val.is_empty() => {
                tracing::debug!("Resolved {section}.{field} from env var {var_name}");
                Ok(SecretValue::from(val))
            }
            _ => Err(SecretResolutionError::NotFound {
                section: section.to_string(),
                field: field.to_string(),
            }),
        }
    }
}

// ─── ConfigBackend ─────────────────────────────────────────

/// Reads secrets from the parsed LatticeConfig struct.
pub(crate) struct ConfigBackend {
    config: LatticeConfig,
}

impl ConfigBackend {
    pub(crate) fn new(config: &LatticeConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Map (section, field) to config struct field value.
    /// Exhaustive match ensures every registry entry has a mapping (SEC-F3).
    fn lookup(&self, section: &str, field: &str) -> Option<String> {
        match (section, field) {
            ("storage", "vast_username") => self.config.storage.vast_username.clone(),
            ("storage", "vast_password") => self.config.storage.vast_password.clone(),
            ("accounting", "waldur_token") => self
                .config
                .accounting
                .as_ref()
                .map(|a| a.waldur_token.clone()),
            ("quorum", "audit_signing_key") => {
                // Binary secret: not available as config literal (use file path instead)
                None
            }
            ("sovra", "key_path") => {
                // Sovra key path handled via file, not as config literal
                None
            }
            _ => None,
        }
    }
}

impl SecretBackend for ConfigBackend {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError> {
        match self.lookup(section, field) {
            Some(val) if !val.is_empty() => {
                tracing::debug!("Resolved {section}.{field} from config file");
                Ok(SecretValue::from(val))
            }
            _ => Err(SecretResolutionError::NotFound {
                section: section.to_string(),
                field: field.to_string(),
            }),
        }
    }
}

// ─── FallbackChain ─────────────────────────────────────────

/// Tries EnvBackend, then ConfigBackend. Used when Vault is not configured.
pub(crate) struct FallbackChain {
    env: EnvBackend,
    config: ConfigBackend,
}

impl FallbackChain {
    pub(crate) fn new(config: &LatticeConfig) -> Self {
        Self {
            env: EnvBackend,
            config: ConfigBackend::new(config),
        }
    }
}

impl SecretBackend for FallbackChain {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError> {
        match self.env.fetch(section, field) {
            Ok(v) => Ok(v),
            Err(SecretResolutionError::NotFound { .. }) => self.config.fetch(section, field),
            Err(e) => Err(e),
        }
    }
}

// ─── MockBackend (testing) ─────────────────────────────────

#[cfg(test)]
pub(crate) struct MockBackend {
    pub(crate) secrets: HashMap<(String, String), String>,
}

#[cfg(test)]
impl SecretBackend for MockBackend {
    fn fetch(&self, section: &str, field: &str) -> Result<SecretValue, SecretResolutionError> {
        match self.secrets.get(&(section.to_string(), field.to_string())) {
            Some(val) => Ok(SecretValue::from(val.as_str())),
            None => Err(SecretResolutionError::NotFound {
                section: section.to_string(),
                field: field.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_backend_returns_not_found_for_unset_var() {
        let result = EnvBackend.fetch("test_nonexistent_xyz_98765", "field_abc");
        assert!(matches!(
            result,
            Err(SecretResolutionError::NotFound { .. })
        ));
    }

    #[test]
    fn config_backend_returns_not_found_for_empty() {
        let config = LatticeConfig::default();
        let backend = ConfigBackend::new(&config);
        let result = backend.fetch("storage", "vast_username");
        assert!(matches!(
            result,
            Err(SecretResolutionError::NotFound { .. })
        ));
    }

    #[test]
    fn config_backend_returns_value_when_set() {
        let mut config = LatticeConfig::default();
        config.storage.vast_username = Some("test-user".to_string());
        let backend = ConfigBackend::new(&config);
        let result = backend.fetch("storage", "vast_username").unwrap();
        assert_eq!(result.expose(), "test-user");
    }

    #[test]
    fn config_backend_returns_not_found_for_unknown_field() {
        let config = LatticeConfig::default();
        let backend = ConfigBackend::new(&config);
        let result = backend.fetch("unknown", "field");
        assert!(matches!(
            result,
            Err(SecretResolutionError::NotFound { .. })
        ));
    }

    #[test]
    fn mock_backend_works() {
        let mut secrets = HashMap::new();
        secrets.insert(
            ("storage".to_string(), "vast_password".to_string()),
            "mock-pw".to_string(),
        );
        let backend = MockBackend { secrets };
        assert_eq!(
            backend.fetch("storage", "vast_password").unwrap().expose(),
            "mock-pw"
        );
        assert!(backend.fetch("storage", "vast_username").is_err());
    }
}
