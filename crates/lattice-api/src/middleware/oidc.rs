//! OIDC token validation middleware.
//!
//! Provides `OidcValidator` trait and supporting types for validating
//! Bearer tokens issued by an OIDC provider. A `StubOidcValidator` is
//! included for use in tests.

use async_trait::async_trait;
use thiserror::Error;

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for OIDC token validation.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Base URL of the OIDC issuer (e.g. `https://auth.example.com`).
    pub issuer_url: String,
    /// Expected `aud` claim value.
    pub audience: String,
    /// Scopes that must all be present in the token.
    pub required_scopes: Vec<String>,
}

// ─── Claims ──────────────────────────────────────────────────────────────────

/// Validated claims extracted from an OIDC token.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenClaims {
    /// Subject (user identifier).
    pub sub: String,
    /// Expiry as Unix timestamp.
    pub exp: i64,
    /// Issuer URL.
    pub iss: String,
    /// Audience.
    pub aud: String,
    /// Space-separated scopes present in the token.
    pub scopes: Vec<String>,
}

// ─── Error ───────────────────────────────────────────────────────────────────

/// Errors that can occur during OIDC token validation.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum OidcError {
    /// The token is malformed or its signature is invalid.
    #[error("invalid token")]
    InvalidToken,
    /// The token's `exp` claim is in the past.
    #[error("token has expired")]
    Expired,
    /// The token's `iss` claim does not match the configured issuer.
    #[error("wrong issuer")]
    WrongIssuer,
    /// One or more required scopes are absent from the token.
    #[error("missing required scope: {0}")]
    MissingScope(String),
    /// The token introspection endpoint returned an error.
    #[error("token introspection failed: {0}")]
    IntrospectionFailed(String),
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Validates a Bearer token and returns the extracted claims.
#[async_trait]
pub trait OidcValidator: Send + Sync {
    /// Parse and validate `token`, returning the set of claims on success.
    async fn validate_token(&self, token: &str) -> Result<TokenClaims, OidcError>;
}

// ─── Stub implementation ──────────────────────────────────────────────────────

/// A test-only validator that accepts any token whose value starts with
/// `"valid-"`.  The remainder of the token string is treated as the subject.
///
/// Tokens that start with `"expired-"` return [`OidcError::Expired`].
/// Tokens that start with `"wrong-issuer-"` return [`OidcError::WrongIssuer`].
/// Tokens that start with `"no-scope-"` return [`OidcError::MissingScope`].
/// All other tokens return [`OidcError::InvalidToken`].
#[derive(Debug, Clone)]
pub struct StubOidcValidator {
    /// Config used to populate the returned claims.
    pub config: OidcConfig,
}

impl StubOidcValidator {
    /// Create a new stub validator from the given config.
    pub fn new(config: OidcConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl OidcValidator for StubOidcValidator {
    async fn validate_token(&self, token: &str) -> Result<TokenClaims, OidcError> {
        if token.starts_with("expired-") {
            return Err(OidcError::Expired);
        }
        if token.starts_with("wrong-issuer-") {
            return Err(OidcError::WrongIssuer);
        }
        if token.starts_with("no-scope-") {
            let missing = token
                .strip_prefix("no-scope-")
                .unwrap_or("read")
                .to_string();
            return Err(OidcError::MissingScope(missing));
        }
        if let Some(subject) = token.strip_prefix("valid-") {
            return Ok(TokenClaims {
                sub: subject.to_string(),
                exp: i64::MAX,
                iss: self.config.issuer_url.clone(),
                aud: self.config.audience.clone(),
                scopes: self.config.required_scopes.clone(),
            });
        }
        Err(OidcError::InvalidToken)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_config() -> OidcConfig {
        OidcConfig {
            issuer_url: "https://auth.example.com".to_string(),
            audience: "lattice-api".to_string(),
            required_scopes: vec!["jobs:read".to_string(), "jobs:write".to_string()],
        }
    }

    fn stub() -> StubOidcValidator {
        StubOidcValidator::new(stub_config())
    }

    // 1. A token with the "valid-" prefix is accepted.
    #[tokio::test]
    async fn valid_token_is_accepted() {
        let claims = stub().validate_token("valid-alice").await.unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.iss, "https://auth.example.com");
        assert_eq!(claims.aud, "lattice-api");
        assert_eq!(claims.exp, i64::MAX);
        assert_eq!(claims.scopes, vec!["jobs:read", "jobs:write"]);
    }

    // 2. A random string (no known prefix) is rejected as InvalidToken.
    #[tokio::test]
    async fn invalid_token_is_rejected() {
        let err = stub().validate_token("garbage-token").await.unwrap_err();
        assert_eq!(err, OidcError::InvalidToken);
    }

    // 3. An "expired-" token is rejected with Expired.
    #[tokio::test]
    async fn expired_token_is_rejected() {
        let err = stub().validate_token("expired-alice").await.unwrap_err();
        assert_eq!(err, OidcError::Expired);
    }

    // 4. A "wrong-issuer-" token is rejected with WrongIssuer.
    #[tokio::test]
    async fn wrong_issuer_token_is_rejected() {
        let err = stub()
            .validate_token("wrong-issuer-alice")
            .await
            .unwrap_err();
        assert_eq!(err, OidcError::WrongIssuer);
    }

    // 5. A "no-scope-<name>" token is rejected with MissingScope(<name>).
    #[tokio::test]
    async fn missing_scope_is_rejected() {
        let err = stub()
            .validate_token("no-scope-jobs:admin")
            .await
            .unwrap_err();
        assert_eq!(err, OidcError::MissingScope("jobs:admin".to_string()));
    }

    // 6. The stub validator populates all claim fields from config.
    #[tokio::test]
    async fn stub_claims_match_config() {
        let config = OidcConfig {
            issuer_url: "https://idp.corp".to_string(),
            audience: "my-service".to_string(),
            required_scopes: vec!["openid".to_string()],
        };
        let validator = StubOidcValidator::new(config.clone());
        let claims = validator.validate_token("valid-bob").await.unwrap();
        assert_eq!(claims.iss, config.issuer_url);
        assert_eq!(claims.aud, config.audience);
        assert_eq!(claims.scopes, config.required_scopes);
        assert_eq!(claims.sub, "bob");
    }

    // 7. Empty string token is rejected.
    #[tokio::test]
    async fn empty_token_is_rejected() {
        let err = stub().validate_token("").await.unwrap_err();
        assert_eq!(err, OidcError::InvalidToken);
    }

    // 8. OidcError Display messages are meaningful.
    #[test]
    fn error_display_messages() {
        assert_eq!(OidcError::InvalidToken.to_string(), "invalid token");
        assert_eq!(OidcError::Expired.to_string(), "token has expired");
        assert_eq!(OidcError::WrongIssuer.to_string(), "wrong issuer");
        assert_eq!(
            OidcError::MissingScope("read".to_string()).to_string(),
            "missing required scope: read"
        );
        assert_eq!(
            OidcError::IntrospectionFailed("timeout".to_string()).to_string(),
            "token introspection failed: timeout"
        );
    }
}
