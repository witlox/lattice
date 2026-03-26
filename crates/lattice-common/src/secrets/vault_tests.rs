//! Integration tests for VaultBackend using wiremock.
//!
//! These tests exercise the full Vault HTTP path: AppRole login, KV v2 reads,
//! and all failure modes (unreachable, auth failure, missing path, missing key).
//! Uses wiremock to mock the Vault HTTP API.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::VaultConfig;
    use crate::secrets::backends::{SecretBackend, VaultBackend};
    use crate::secrets::errors::SecretResolutionError;

    /// Helper: create a VaultConfig pointing to the given mock server.
    fn vault_config(server_uri: &str) -> VaultConfig {
        VaultConfig {
            address: server_uri.to_string(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role-id".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        }
    }

    /// Helper: mount AppRole login mock returning a token.
    async fn mount_approle_login(server: &MockServer, token: &str) {
        let body = serde_json::json!({
            "auth": {
                "client_token": token,
                "accessor": "test-accessor",
                "policies": ["default"],
                "lease_duration": 3600,
                "renewable": true
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/auth/approle/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// Helper: mount a KV v2 read response.
    async fn mount_kv_read(server: &MockServer, kv_path: &str, data: HashMap<&str, &str>) {
        let data_map: serde_json::Value = data
            .into_iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect();
        let body = serde_json::json!({
            "data": {
                "data": data_map,
                "metadata": {
                    "version": 1,
                    "created_time": "2026-01-01T00:00:00Z"
                }
            }
        });
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kv_path}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    // ── Success path ──────────────────────────────────────────

    #[tokio::test]
    async fn vault_backend_authenticates_and_fetches() {
        let server = MockServer::start().await;
        mount_approle_login(&server, "s.test-token").await;

        let mut data = HashMap::new();
        data.insert("vast_password", "vault-secret-pw");
        data.insert("vast_username", "vault-user");
        mount_kv_read(&server, "secret/data/lattice/storage", data).await;

        std::env::set_var("TEST_VAULT_SECRET_ID", "test-secret-id");

        let config = vault_config(&server.uri());
        // VaultBackend::new uses blocking HTTP — run in spawn_blocking
        let backend = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap()
            .unwrap();

        let result = tokio::task::spawn_blocking(move || backend.fetch("storage", "vast_password"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.expose(), "vault-secret-pw");

        std::env::remove_var("TEST_VAULT_SECRET_ID");
    }

    // ── Auth failure ──────────────────────────────────────────

    #[tokio::test]
    async fn vault_backend_auth_failure() {
        let server = MockServer::start().await;

        // Return 403 for AppRole login
        Mock::given(method("POST"))
            .and(path("/v1/auth/approle/login"))
            .respond_with(ResponseTemplate::new(403).set_body_string("permission denied"))
            .mount(&server)
            .await;

        std::env::set_var("TEST_VAULT_SECRET_ID_AUTH", "bad-secret");

        let config = VaultConfig {
            address: server.uri(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "bad-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_AUTH".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let result = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap();

        assert!(matches!(
            result,
            Err(SecretResolutionError::AuthenticationFailed { .. })
        ));

        std::env::remove_var("TEST_VAULT_SECRET_ID_AUTH");
    }

    // ── Connection failure (unreachable) ──────────────────────

    #[tokio::test]
    async fn vault_backend_connection_refused() {
        // Use a port that's almost certainly not listening
        std::env::set_var("TEST_VAULT_SECRET_ID_CONN", "test-secret");

        let config = VaultConfig {
            address: "http://127.0.0.1:19999".to_string(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_CONN".to_string(),
            tls_ca_path: None,
            timeout_secs: 2,
        };

        let result = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap();

        assert!(matches!(
            result,
            Err(SecretResolutionError::ConnectionFailed { .. })
        ));

        std::env::remove_var("TEST_VAULT_SECRET_ID_CONN");
    }

    // ── Missing secret_id env var ─────────────────────────────

    #[tokio::test]
    async fn vault_backend_missing_secret_id_env() {
        std::env::remove_var("TEST_VAULT_MISSING_ENV");

        let config = VaultConfig {
            address: "http://localhost:8200".to_string(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_MISSING_ENV".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let result = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap();

        assert!(matches!(
            result,
            Err(SecretResolutionError::SecretIdEnvMissing { .. })
        ));
    }

    // ── Missing path (404) ────────────────────────────────────

    #[tokio::test]
    async fn vault_backend_path_not_found() {
        let server = MockServer::start().await;
        mount_approle_login(&server, "s.test-token").await;

        // Return 404 for the KV read
        Mock::given(method("GET"))
            .and(path("/v1/secret/data/lattice/storage"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        std::env::set_var("TEST_VAULT_SECRET_ID_404", "test-secret");

        let config = VaultConfig {
            address: server.uri(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_404".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let backend = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap()
            .unwrap();

        let result = tokio::task::spawn_blocking(move || backend.fetch("storage", "vast_password"))
            .await
            .unwrap();

        assert!(matches!(
            result,
            Err(SecretResolutionError::PathNotFound { .. })
        ));

        std::env::remove_var("TEST_VAULT_SECRET_ID_404");
    }

    // ── Missing key (path exists but key absent) ──────────────

    #[tokio::test]
    async fn vault_backend_key_not_found() {
        let server = MockServer::start().await;
        mount_approle_login(&server, "s.test-token").await;

        // Return a valid KV response but WITHOUT the requested key
        let mut data = HashMap::new();
        data.insert("vast_username", "exists");
        // vast_password is NOT in the response
        mount_kv_read(&server, "secret/data/lattice/storage", data).await;

        std::env::set_var("TEST_VAULT_SECRET_ID_KEY", "test-secret");

        let config = VaultConfig {
            address: server.uri(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_KEY".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let backend = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap()
            .unwrap();

        let result = tokio::task::spawn_blocking(move || backend.fetch("storage", "vast_password"))
            .await
            .unwrap();

        assert!(matches!(
            result,
            Err(SecretResolutionError::KeyNotFound { .. })
        ));

        std::env::remove_var("TEST_VAULT_SECRET_ID_KEY");
    }

    // ── Vault server error (500) ──────────────────────────────

    #[tokio::test]
    async fn vault_backend_server_error() {
        let server = MockServer::start().await;
        mount_approle_login(&server, "s.test-token").await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/lattice/storage"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        std::env::set_var("TEST_VAULT_SECRET_ID_500", "test-secret");

        let config = VaultConfig {
            address: server.uri(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_500".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let backend = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap()
            .unwrap();

        let result = tokio::task::spawn_blocking(move || backend.fetch("storage", "vast_password"))
            .await
            .unwrap();

        match result {
            Err(SecretResolutionError::VaultError { status, .. }) => {
                assert_eq!(status, 500);
            }
            other => panic!("Expected VaultError, got {other:?}"),
        }

        std::env::remove_var("TEST_VAULT_SECRET_ID_500");
    }

    // ── Error message content (GAP-SEC-4) ─────────────────────

    #[tokio::test]
    async fn vault_error_messages_contain_path_info() {
        let server = MockServer::start().await;
        mount_approle_login(&server, "s.test-token").await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/lattice/storage"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        std::env::set_var("TEST_VAULT_SECRET_ID_MSG", "test-secret");

        let config = VaultConfig {
            address: server.uri(),
            prefix: "secret/data/lattice".to_string(),
            role_id: "test-role".to_string(),
            secret_id_env: "TEST_VAULT_SECRET_ID_MSG".to_string(),
            tls_ca_path: None,
            timeout_secs: 5,
        };

        let backend = tokio::task::spawn_blocking(move || VaultBackend::new(&config))
            .await
            .unwrap()
            .unwrap();

        let result = tokio::task::spawn_blocking(move || backend.fetch("storage", "vast_password"))
            .await
            .unwrap();

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("secret/data/lattice/storage"),
            "Error should contain Vault path, got: {err_msg}"
        );

        std::env::remove_var("TEST_VAULT_SECRET_ID_MSG");
    }
}
