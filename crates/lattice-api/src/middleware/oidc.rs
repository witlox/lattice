//! OIDC token validation middleware.
//!
//! Provides `OidcValidator` trait and supporting types for validating
//! Bearer tokens issued by an OIDC provider. A `StubOidcValidator` is
//! included for use in tests.

#[cfg(feature = "oidc")]
use std::sync::Arc;
#[cfg(feature = "oidc")]
use std::time::Instant;

use async_trait::async_trait;
#[cfg(feature = "oidc")]
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
#[cfg(feature = "oidc")]
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "oidc")]
use tokio::sync::RwLock;

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

// ─── JWKS cache ──────────────────────────────────────────────────────────────

/// Cached JWKS keyset with a time-to-live for automatic refresh.
#[cfg(feature = "oidc")]
struct CachedJwks {
    keys: jsonwebtoken::jwk::JwkSet,
    fetched_at: Instant,
}

#[cfg(feature = "oidc")]
impl CachedJwks {
    /// Cache TTL: 1 hour.
    const TTL: std::time::Duration = std::time::Duration::from_secs(3600);

    fn is_expired(&self) -> bool {
        self.fetched_at.elapsed() > Self::TTL
    }
}

// ─── JWT token claims (for deserialization) ──────────────────────────────────

/// Internal claims structure for `jsonwebtoken::decode`.
#[cfg(feature = "oidc")]
#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    sub: String,
    exp: i64,
    iss: String,
    aud: JwtAudience,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// Audience can be a single string or an array of strings.
#[cfg(feature = "oidc")]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum JwtAudience {
    Single(String),
    Multiple(Vec<String>),
}

#[cfg(feature = "oidc")]
impl JwtAudience {
    fn primary(&self) -> String {
        match self {
            JwtAudience::Single(s) => s.clone(),
            JwtAudience::Multiple(v) => v.first().cloned().unwrap_or_default(),
        }
    }
}

// ─── JWT OIDC Validator ──────────────────────────────────────────────────────

/// A real OIDC validator that fetches JWKS from the provider and validates
/// JWT tokens using RS256 or ES256.
///
/// Caches the JWKS for 1 hour and automatically refreshes on cache miss
/// (when a `kid` is not found in the cached keyset).
#[cfg(feature = "oidc")]
pub struct JwtOidcValidator {
    /// OIDC issuer URL (e.g. `https://auth.example.com`).
    issuer_url: String,
    /// Expected audience claim.
    audience: String,
    /// Required scopes.
    required_scopes: Vec<String>,
    /// HTTP client for fetching JWKS.
    client: reqwest::Client,
    /// Cached JWKS keyset.
    jwks_cache: Arc<RwLock<Option<CachedJwks>>>,
}

#[cfg(feature = "oidc")]
impl JwtOidcValidator {
    /// Create a new JWT OIDC validator.
    pub fn new(config: OidcConfig) -> Self {
        Self {
            issuer_url: config.issuer_url,
            audience: config.audience,
            required_scopes: config.required_scopes,
            client: reqwest::Client::new(),
            jwks_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Create with a custom reqwest client (for testing with mock servers).
    pub fn with_client(config: OidcConfig, client: reqwest::Client) -> Self {
        Self {
            issuer_url: config.issuer_url,
            audience: config.audience,
            required_scopes: config.required_scopes,
            client,
            jwks_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Fetch the JWKS URI from the OpenID Connect discovery document.
    async fn fetch_jwks_uri(&self) -> Result<String, OidcError> {
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer_url.trim_end_matches('/')
        );

        let resp =
            self.client.get(&discovery_url).send().await.map_err(|e| {
                OidcError::IntrospectionFailed(format!("discovery fetch failed: {e}"))
            })?;

        if !resp.status().is_success() {
            return Err(OidcError::IntrospectionFailed(format!(
                "discovery returned {}",
                resp.status()
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| OidcError::IntrospectionFailed(format!("discovery parse failed: {e}")))?;

        body["jwks_uri"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| OidcError::IntrospectionFailed("jwks_uri not found in discovery".into()))
    }

    /// Fetch the JWKS from the provider.
    async fn fetch_jwks(&self) -> Result<jsonwebtoken::jwk::JwkSet, OidcError> {
        let jwks_uri = self.fetch_jwks_uri().await?;

        let resp = self
            .client
            .get(&jwks_uri)
            .send()
            .await
            .map_err(|e| OidcError::IntrospectionFailed(format!("JWKS fetch failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(OidcError::IntrospectionFailed(format!(
                "JWKS returned {}",
                resp.status()
            )));
        }

        resp.json()
            .await
            .map_err(|e| OidcError::IntrospectionFailed(format!("JWKS parse failed: {e}")))
    }

    /// Get the cached JWKS, refreshing if expired or empty.
    async fn get_jwks(&self) -> Result<jsonwebtoken::jwk::JwkSet, OidcError> {
        // Fast path: check if cache is valid.
        {
            let cache = self.jwks_cache.read().await;
            if let Some(ref cached) = *cache {
                if !cached.is_expired() {
                    return Ok(cached.keys.clone());
                }
            }
        }

        // Slow path: fetch and cache.
        let keys = self.fetch_jwks().await?;
        let mut cache = self.jwks_cache.write().await;
        *cache = Some(CachedJwks {
            keys: keys.clone(),
            fetched_at: Instant::now(),
        });
        Ok(keys)
    }
}

#[cfg(feature = "oidc")]
#[async_trait]
impl OidcValidator for JwtOidcValidator {
    async fn validate_token(&self, token: &str) -> Result<TokenClaims, OidcError> {
        // 1. Decode the JWT header to get the `kid` and algorithm.
        let header = decode_header(token).map_err(|_| OidcError::InvalidToken)?;

        let kid = header.kid.ok_or(OidcError::InvalidToken)?;
        let alg = header.alg;

        // Only allow RS256 and ES256.
        if !matches!(alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err(OidcError::InvalidToken);
        }

        // 2. Get JWKS and find matching key.
        let jwks = self.get_jwks().await?;
        let jwk = jwks.find(&kid).ok_or(OidcError::InvalidToken)?;

        // 3. Build decoding key from JWK.
        let decoding_key = DecodingKey::from_jwk(jwk).map_err(|_| OidcError::InvalidToken)?;

        // 4. Validate and decode the token.
        let mut validation = Validation::new(alg);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer_url]);
        validation.validate_exp = true;

        let token_data =
            decode::<JwtClaims>(token, &decoding_key, &validation).map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => OidcError::Expired,
                jsonwebtoken::errors::ErrorKind::InvalidIssuer => OidcError::WrongIssuer,
                jsonwebtoken::errors::ErrorKind::InvalidAudience => {
                    OidcError::WrongIssuer // audience mismatch
                }
                _ => OidcError::InvalidToken,
            })?;

        let claims = token_data.claims;

        // 5. Extract scopes (supports both "scope" string and "scopes" array).
        let scopes: Vec<String> = if let Some(scope_str) = &claims.scope {
            scope_str
                .split_whitespace()
                .map(|s| s.to_string())
                .collect()
        } else if let Some(scope_vec) = &claims.scopes {
            scope_vec.clone()
        } else {
            vec![]
        };

        // 6. Check required scopes.
        for required in &self.required_scopes {
            if !scopes.contains(required) {
                return Err(OidcError::MissingScope(required.clone()));
            }
        }

        Ok(TokenClaims {
            sub: claims.sub,
            exp: claims.exp,
            iss: claims.iss,
            aud: claims.aud.primary(),
            scopes,
        })
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

    // ─── JwtOidcValidator tests ───────────────────────────────────────────

    /// Helper: generate an ECDSA P-256 key pair, sign a JWT (ES256), and build a JWKS JSON.
    #[cfg(feature = "oidc")]
    fn build_ec_jwt(
        sub: &str,
        iss: &str,
        aud: &str,
        scopes: &str,
        exp_offset_secs: i64,
    ) -> (String, serde_json::Value) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        use jsonwebtoken::{encode, EncodingKey, Header};

        // Generate ECDSA P-256 key pair using rcgen (default algorithm).
        let key_pair = rcgen::KeyPair::generate().unwrap();

        // Serialize private key as PKCS#8 PEM for jsonwebtoken.
        let private_pem = key_pair.serialize_pem();
        let encoding_key = EncodingKey::from_ec_pem(private_pem.as_bytes()).unwrap();

        let kid = "test-ec-key-1".to_string();
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(kid.clone());

        let now = chrono::Utc::now().timestamp();
        let claims = serde_json::json!({
            "sub": sub,
            "iss": iss,
            "aud": aud,
            "exp": now + exp_offset_secs,
            "iat": now,
            "scope": scopes,
        });

        let token = encode(&header, &claims, &encoding_key).unwrap();

        // Extract EC public key coordinates from SPKI DER for JWKS.
        use rcgen::PublicKeyData;
        let spki_der = key_pair.subject_public_key_info();
        let (x, y) = extract_ec_coordinates_from_spki(&spki_der);

        let jwks = serde_json::json!({
            "keys": [{
                "kty": "EC",
                "kid": kid,
                "use": "sig",
                "alg": "ES256",
                "crv": "P-256",
                "x": URL_SAFE_NO_PAD.encode(&x),
                "y": URL_SAFE_NO_PAD.encode(&y),
            }]
        });

        (token, jwks)
    }

    /// Extract x and y coordinates from an EC SPKI DER public key.
    /// For P-256, the public key is 65 bytes: 0x04 || x (32 bytes) || y (32 bytes).
    #[cfg(feature = "oidc")]
    fn extract_ec_coordinates_from_spki(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
        // The uncompressed EC point is at the end of the SPKI structure.
        // Find the BIT STRING containing the public key.
        // For P-256 SPKI, the point is the last 65 bytes inside the BIT STRING.
        // Look for the 0x04 marker (uncompressed point).
        let point_start = der
            .windows(1)
            .rposition(|w| {
                w[0] == 0x04 && der.len() - der.iter().rposition(|&b| b == 0x04).unwrap() == 65
            })
            .unwrap_or_else(|| {
                // Fallback: scan for the uncompressed point marker with correct remaining length.
                for i in 0..der.len() {
                    if der[i] == 0x04 && i + 65 == der.len() {
                        return i;
                    }
                }
                panic!("Could not find EC point in SPKI DER");
            });

        let x = der[point_start + 1..point_start + 33].to_vec();
        let y = der[point_start + 33..point_start + 65].to_vec();
        (x, y)
    }

    /// Helper to create a wiremock-based OIDC server and validator.
    #[cfg(feature = "oidc")]
    async fn setup_jwt_validator(
        scopes: Vec<String>,
    ) -> (wiremock::MockServer, JwtOidcValidator, String) {
        let server = wiremock::MockServer::start().await;
        let issuer_url = server.uri();
        let audience = "lattice-api".to_string();

        let (token, jwks) = build_ec_jwt("alice", &issuer_url, &audience, &scopes.join(" "), 3600);

        // Mount OIDC discovery endpoint.
        let discovery = serde_json::json!({
            "issuer": issuer_url,
            "jwks_uri": format!("{}/jwks", issuer_url),
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/.well-known/openid-configuration",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&discovery))
            .mount(&server)
            .await;

        // Mount JWKS endpoint.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/jwks"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&jwks))
            .mount(&server)
            .await;

        let config = OidcConfig {
            issuer_url,
            audience,
            required_scopes: scopes,
        };
        let validator = JwtOidcValidator::new(config);

        (server, validator, token)
    }

    // 9. JwtOidcValidator accepts a valid RS256 token.
    #[cfg(feature = "oidc")]
    #[tokio::test]
    async fn jwt_validator_accepts_valid_token() {
        let (_server, validator, token) = setup_jwt_validator(vec!["jobs:read".to_string()]).await;

        let claims = validator.validate_token(&token).await.unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.aud, "lattice-api");
        assert!(claims.scopes.contains(&"jobs:read".to_string()));
    }

    // 10. JwtOidcValidator rejects an expired token.
    #[cfg(feature = "oidc")]
    #[tokio::test]
    async fn jwt_validator_rejects_expired_token() {
        let server = wiremock::MockServer::start().await;
        let issuer_url = server.uri();

        let (token, jwks) = build_ec_jwt("alice", &issuer_url, "lattice-api", "jobs:read", -3600);

        let discovery = serde_json::json!({
            "issuer": issuer_url,
            "jwks_uri": format!("{}/jwks", issuer_url),
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/.well-known/openid-configuration",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&discovery))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/jwks"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&jwks))
            .mount(&server)
            .await;

        let config = OidcConfig {
            issuer_url,
            audience: "lattice-api".to_string(),
            required_scopes: vec![],
        };
        let validator = JwtOidcValidator::new(config);
        let err = validator.validate_token(&token).await.unwrap_err();
        assert_eq!(err, OidcError::Expired);
    }

    // 11. JwtOidcValidator rejects a token missing required scopes.
    #[cfg(feature = "oidc")]
    #[tokio::test]
    async fn jwt_validator_rejects_missing_scope() {
        let server = wiremock::MockServer::start().await;
        let issuer_url = server.uri();
        let (token, jwks) = build_ec_jwt("alice", &issuer_url, "lattice-api", "jobs:read", 3600);

        let discovery = serde_json::json!({
            "issuer": issuer_url,
            "jwks_uri": format!("{}/jwks", issuer_url),
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/.well-known/openid-configuration",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&discovery))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/jwks"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&jwks))
            .mount(&server)
            .await;

        let config = OidcConfig {
            issuer_url,
            audience: "lattice-api".to_string(),
            required_scopes: vec!["admin:write".to_string()],
        };
        let validator = JwtOidcValidator::new(config);
        let err = validator.validate_token(&token).await.unwrap_err();
        assert_eq!(err, OidcError::MissingScope("admin:write".to_string()));
    }

    // 12. JwtOidcValidator rejects a garbage token.
    #[cfg(feature = "oidc")]
    #[tokio::test]
    async fn jwt_validator_rejects_garbage_token() {
        let (_server, validator, _) = setup_jwt_validator(vec!["jobs:read".to_string()]).await;

        let err = validator.validate_token("not.a.jwt").await.unwrap_err();
        assert_eq!(err, OidcError::InvalidToken);
    }

    // 13. JwtOidcValidator caches JWKS across calls.
    #[cfg(feature = "oidc")]
    #[tokio::test]
    async fn jwt_validator_caches_jwks() {
        let (_server, validator, token) = setup_jwt_validator(vec!["jobs:read".to_string()]).await;

        // First call populates cache.
        let _ = validator.validate_token(&token).await.unwrap();

        // Second call uses cache.
        let claims = validator.validate_token(&token).await.unwrap();
        assert_eq!(claims.sub, "alice");
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
