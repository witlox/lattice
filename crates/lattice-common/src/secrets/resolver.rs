use base64::Engine;
use zeroize::Zeroizing;

use super::backends::{FallbackChain, SecretBackend, VaultBackend};
use super::errors::SecretResolutionError;
use super::value::SecretValue;
use crate::config::LatticeConfig;

/// Which features require which secrets.
#[derive(Debug, Clone, Copy)]
enum FeatureCondition {
    StorageConfigured,
    AccountingConfigured,
    FederationConfigured,
    Always,
}

/// A secret field definition in the registry.
struct SecretField {
    section: &'static str,
    field: &'static str,
    required_when: FeatureCondition,
}

/// Compile-time registry of all secret fields (INV-SEC5).
const SECRETS: &[SecretField] = &[
    SecretField {
        section: "storage",
        field: "vast_username",
        required_when: FeatureCondition::StorageConfigured,
    },
    SecretField {
        section: "storage",
        field: "vast_password",
        required_when: FeatureCondition::StorageConfigured,
    },
    SecretField {
        section: "accounting",
        field: "waldur_token",
        required_when: FeatureCondition::AccountingConfigured,
    },
    SecretField {
        section: "quorum",
        field: "audit_signing_key",
        required_when: FeatureCondition::Always,
    },
    SecretField {
        section: "sovra",
        field: "key_path",
        required_when: FeatureCondition::FederationConfigured,
    },
];

/// Output of secret resolution. Consumed by component initialization code.
pub struct ResolvedSecrets {
    pub vast_username: Option<SecretValue>,
    pub vast_password: Option<SecretValue>,
    pub waldur_token: Option<SecretValue>,
    /// Ed25519 signing key seed, decoded from base64. Zeroized on drop.
    pub audit_signing_key: Option<Zeroizing<[u8; 32]>>,
    pub sovra_key: Option<SecretValue>,
}

/// Resolves all operational secrets at startup.
///
/// When Vault is configured, all secrets come from Vault KV v2 (INV-SEC4).
/// When Vault is not configured, falls back to env vars -> config literals (INV-SEC6).
pub struct SecretResolver {
    backend: Box<dyn SecretBackend>,
    vault_active: bool,
}

impl SecretResolver {
    /// Construct the resolver. If Vault is configured, authenticates immediately.
    /// Failure is fatal (INV-SEC3).
    pub fn new(config: &LatticeConfig) -> Result<Self, SecretResolutionError> {
        if let Some(ref vault_config) = config.vault {
            let backend = VaultBackend::new(vault_config)?;
            Ok(Self {
                backend: Box::new(backend),
                vault_active: true,
            })
        } else {
            tracing::info!("Secret resolution via environment/config (no Vault configured)");
            Ok(Self {
                backend: Box::new(FallbackChain::new(config)),
                vault_active: false,
            })
        }
    }

    /// Construct with a custom backend (for testing).
    #[cfg(test)]
    fn with_backend(backend: Box<dyn SecretBackend>, vault_active: bool) -> Self {
        Self {
            backend,
            vault_active,
        }
    }

    /// Resolve all registered secrets. Collects ALL errors before returning (SEC-F8).
    /// Any error is fatal (INV-SEC1).
    pub fn resolve_all(
        &self,
        config: &LatticeConfig,
    ) -> Result<ResolvedSecrets, SecretResolutionError> {
        let mut vast_username: Option<SecretValue> = None;
        let mut vast_password: Option<SecretValue> = None;
        let mut waldur_token: Option<SecretValue> = None;
        let mut audit_signing_key_raw: Option<SecretValue> = None;
        let mut sovra_key: Option<SecretValue> = None;
        let mut errors: Vec<SecretResolutionError> = Vec::new();

        for secret in SECRETS {
            if !self.is_required(secret, config) {
                continue;
            }

            match self.backend.fetch(secret.section, secret.field) {
                Ok(value) => match (secret.section, secret.field) {
                    ("storage", "vast_username") => vast_username = Some(value),
                    ("storage", "vast_password") => vast_password = Some(value),
                    ("accounting", "waldur_token") => waldur_token = Some(value),
                    ("quorum", "audit_signing_key") => audit_signing_key_raw = Some(value),
                    ("sovra", "key_path") => sovra_key = Some(value),
                    _ => {}
                },
                Err(e) => {
                    // audit_signing_key is special: NotFound is OK (dev mode fallback)
                    // but only when Vault is NOT configured (INV-SEC4: Vault = fatal if missing).
                    if secret.field == "audit_signing_key"
                        && !self.vault_active
                        && matches!(e, SecretResolutionError::NotFound { .. })
                    {
                        continue;
                    }
                    errors.push(e);
                }
            }
        }

        if !errors.is_empty() {
            return if errors.len() == 1 {
                Err(errors.remove(0))
            } else {
                Err(SecretResolutionError::Multiple(errors))
            };
        }

        // Decode audit signing key from base64 if present
        let audit_signing_key = match audit_signing_key_raw {
            Some(ref raw) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(raw.expose())
                    .map_err(|e| SecretResolutionError::InvalidFormat {
                        section: "quorum".to_string(),
                        field: "audit_signing_key".to_string(),
                        reason: format!("base64 decode failed: {e}"),
                    })?;
                if decoded.len() < 32 {
                    return Err(SecretResolutionError::InvalidFormat {
                        section: "quorum".to_string(),
                        field: "audit_signing_key".to_string(),
                        reason: format!(
                            "decoded key must be at least 32 bytes, got {}",
                            decoded.len()
                        ),
                    });
                }
                let mut seed = Zeroizing::new([0u8; 32]);
                seed.copy_from_slice(&decoded[..32]);
                Some(seed)
            }
            None => None,
        };

        Ok(ResolvedSecrets {
            vast_username,
            vast_password,
            waldur_token,
            audit_signing_key,
            sovra_key,
        })
    }

    /// Check whether a secret field is required given the current config.
    fn is_required(&self, secret: &SecretField, config: &LatticeConfig) -> bool {
        match secret.required_when {
            FeatureCondition::StorageConfigured => config.storage.vast_api_url.is_some(),
            FeatureCondition::AccountingConfigured => {
                config.accounting.as_ref().is_some_and(|a| a.enabled)
            }
            FeatureCondition::FederationConfigured => config.federation.is_some(),
            FeatureCondition::Always => true,
        }
    }

    /// Whether Vault is the active backend.
    pub fn is_vault_active(&self) -> bool {
        self.vault_active
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::secrets::backends::MockBackend;

    fn mock_resolver(
        secrets: HashMap<(String, String), String>,
        vault_active: bool,
    ) -> SecretResolver {
        SecretResolver::with_backend(Box::new(MockBackend { secrets }), vault_active)
    }

    #[test]
    fn resolver_without_vault_uses_fallback() {
        let config = LatticeConfig::default();
        let resolver = SecretResolver::new(&config).unwrap();
        assert!(!resolver.is_vault_active());
    }

    #[test]
    fn resolve_all_succeeds_with_no_features_configured() {
        let config = LatticeConfig::default();
        let resolver = mock_resolver(HashMap::new(), false);
        let secrets = resolver.resolve_all(&config).unwrap();
        assert!(secrets.vast_username.is_none());
        assert!(secrets.vast_password.is_none());
        assert!(secrets.waldur_token.is_none());
        assert!(secrets.audit_signing_key.is_none());
        assert!(secrets.sovra_key.is_none());
    }

    #[test]
    fn resolve_all_returns_storage_secrets() {
        let mut config = LatticeConfig::default();
        config.storage.vast_api_url = Some("https://vast.example.com".to_string());

        let mut secrets = HashMap::new();
        secrets.insert(
            ("storage".to_string(), "vast_username".to_string()),
            "user1".to_string(),
        );
        secrets.insert(
            ("storage".to_string(), "vast_password".to_string()),
            "pass1".to_string(),
        );

        let resolver = mock_resolver(secrets, false);
        let resolved = resolver.resolve_all(&config).unwrap();

        assert_eq!(resolved.vast_username.unwrap().expose(), "user1");
        assert_eq!(resolved.vast_password.unwrap().expose(), "pass1");
    }

    #[test]
    fn resolve_all_fails_when_required_storage_secret_missing() {
        let mut config = LatticeConfig::default();
        config.storage.vast_api_url = Some("https://vast.example.com".to_string());

        // Only provide username, not password
        let mut secrets = HashMap::new();
        secrets.insert(
            ("storage".to_string(), "vast_username".to_string()),
            "user1".to_string(),
        );

        let resolver = mock_resolver(secrets, false);
        let result = resolver.resolve_all(&config);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_all_collects_multiple_errors() {
        let mut config = LatticeConfig::default();
        config.storage.vast_api_url = Some("https://vast.example.com".to_string());

        // Provide neither username nor password
        let resolver = mock_resolver(HashMap::new(), false);
        let result = resolver.resolve_all(&config);
        match result {
            Err(SecretResolutionError::Multiple(errors)) => {
                assert_eq!(errors.len(), 2);
            }
            _ => panic!("Expected Multiple error"),
        }
    }

    #[test]
    fn audit_signing_key_decoded_from_base64() {
        let key_bytes = [42u8; 32];
        let encoded = base64::engine::general_purpose::STANDARD.encode(key_bytes);

        let mut secrets = HashMap::new();
        secrets.insert(
            ("quorum".to_string(), "audit_signing_key".to_string()),
            encoded,
        );

        let config = LatticeConfig::default();
        let resolver = mock_resolver(secrets, false);
        let resolved = resolver.resolve_all(&config).unwrap();

        let key = resolved.audit_signing_key.unwrap();
        assert_eq!(&*key, &key_bytes);
    }

    #[test]
    fn audit_signing_key_invalid_base64_fails() {
        let mut secrets = HashMap::new();
        secrets.insert(
            ("quorum".to_string(), "audit_signing_key".to_string()),
            "not-valid-base64!!!".to_string(),
        );

        let config = LatticeConfig::default();
        let resolver = mock_resolver(secrets, false);
        let result = resolver.resolve_all(&config);
        assert!(matches!(
            result,
            Err(SecretResolutionError::InvalidFormat { .. })
        ));
    }

    #[test]
    fn audit_signing_key_too_short_fails() {
        let short_key = [42u8; 16];
        let encoded = base64::engine::general_purpose::STANDARD.encode(short_key);

        let mut secrets = HashMap::new();
        secrets.insert(
            ("quorum".to_string(), "audit_signing_key".to_string()),
            encoded,
        );

        let config = LatticeConfig::default();
        let resolver = mock_resolver(secrets, false);
        let result = resolver.resolve_all(&config);
        assert!(matches!(
            result,
            Err(SecretResolutionError::InvalidFormat { .. })
        ));
    }

    #[test]
    fn audit_signing_key_absent_ok_without_vault() {
        // No audit_signing_key in mock, Vault NOT active -> dev mode, allowed
        let config = LatticeConfig::default();
        let resolver = mock_resolver(HashMap::new(), false);
        let resolved = resolver.resolve_all(&config).unwrap();
        assert!(resolved.audit_signing_key.is_none());
    }

    #[test]
    fn audit_signing_key_absent_fatal_with_vault() {
        // No audit_signing_key in mock, Vault IS active -> must fail (INV-SEC4)
        let config = LatticeConfig::default();
        let resolver = mock_resolver(HashMap::new(), true);
        let result = resolver.resolve_all(&config);
        assert!(result.is_err());
    }

    #[test]
    fn vault_override_ignores_config_literals() {
        // When vault_active=true, only the mock backend matters.
        // Config literals are irrelevant.
        let mut config = LatticeConfig::default();
        config.storage.vast_api_url = Some("https://vast.example.com".to_string());
        config.storage.vast_username = Some("config-value".to_string());

        let mut secrets = HashMap::new();
        secrets.insert(
            ("storage".to_string(), "vast_username".to_string()),
            "vault-value".to_string(),
        );
        secrets.insert(
            ("storage".to_string(), "vast_password".to_string()),
            "vault-pass".to_string(),
        );
        // Need audit key too since vault is active
        let key_bytes = [42u8; 32];
        secrets.insert(
            ("quorum".to_string(), "audit_signing_key".to_string()),
            base64::engine::general_purpose::STANDARD.encode(key_bytes),
        );

        let resolver = mock_resolver(secrets, true);
        let resolved = resolver.resolve_all(&config).unwrap();
        assert_eq!(resolved.vast_username.unwrap().expose(), "vault-value");
    }

    #[test]
    fn convention_path_mapping() {
        assert_eq!(SECRETS[0].section, "storage");
        assert_eq!(SECRETS[0].field, "vast_username");
        assert_eq!(SECRETS[1].section, "storage");
        assert_eq!(SECRETS[1].field, "vast_password");
        assert_eq!(SECRETS[2].section, "accounting");
        assert_eq!(SECRETS[2].field, "waldur_token");
        assert_eq!(SECRETS[3].section, "quorum");
        assert_eq!(SECRETS[3].field, "audit_signing_key");
        assert_eq!(SECRETS[4].section, "sovra");
        assert_eq!(SECRETS[4].field, "key_path");
    }

    #[test]
    fn accounting_resolved_when_enabled() {
        let mut config = LatticeConfig::default();
        config.accounting = Some(crate::config::AccountingConfig {
            enabled: true,
            waldur_api_url: "https://waldur.example.com".to_string(),
            waldur_token: String::new(),
            push_interval_seconds: 60,
            buffer_size: 1000,
        });

        let mut secrets = HashMap::new();
        secrets.insert(
            ("accounting".to_string(), "waldur_token".to_string()),
            "tok-abc".to_string(),
        );

        let resolver = mock_resolver(secrets, false);
        let resolved = resolver.resolve_all(&config).unwrap();
        assert_eq!(resolved.waldur_token.unwrap().expose(), "tok-abc");
    }
}
