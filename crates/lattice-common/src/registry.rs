//! OCI distribution spec image resolver.
//!
//! Talks to OCI-compliant registries (Docker Hub, JFrog, GHCR, ORAS) to
//! resolve image references to content-addressed digests.

use async_trait::async_trait;
use std::time::Duration;

use crate::traits::{ImageResolveError, ImageResolver};
use crate::types::{ImageMetadata, ImageRef, ImageType};

/// Accept header for OCI distribution spec manifest requests.
const OCI_ACCEPT_HEADER: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// An [`ImageResolver`] that talks to OCI distribution spec registries.
///
/// Resolves human-readable image specs (e.g., `registry.io/name:tag`) to
/// content-addressed [`ImageRef`]s by querying the registry manifest API.
pub struct OciRegistryResolver {
    client: reqwest::Client,
    default_registry: String,
}

/// Components parsed from an image spec string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageSpec {
    registry: String,
    name: String,
    /// Tag or digest reference (e.g., "latest" or "sha256:abc123").
    reference: String,
    /// True when the reference is a digest (contains `@sha256:`).
    is_digest: bool,
}

impl OciRegistryResolver {
    /// Create a new resolver with the given default registry.
    ///
    /// The default registry is used when an image spec does not include a
    /// registry prefix (e.g., `myimage:latest` becomes
    /// `<default_registry>/myimage:latest`).
    pub fn new(default_registry: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            default_registry: default_registry.to_string(),
        }
    }

    /// Create a resolver with a pre-built client (useful for testing).
    #[cfg(test)]
    fn with_client(client: reqwest::Client, default_registry: &str) -> Self {
        Self {
            client,
            default_registry: default_registry.to_string(),
        }
    }

    /// Parse an image spec string into its components.
    ///
    /// Accepted formats:
    /// - `registry.io/name:tag`
    /// - `registry.io/org/name:tag`
    /// - `name:tag` (uses default registry)
    /// - `name@sha256:abc123` (digest reference)
    /// - `registry.io/name@sha256:abc123`
    fn parse_spec(&self, spec: &str) -> Result<ImageSpec, ImageResolveError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(ImageResolveError::InvalidSpec {
                spec: spec.to_string(),
                reason: "empty image spec".to_string(),
            });
        }

        // Check for digest reference (@sha256:...)
        if let Some(at_pos) = spec.find('@') {
            let before_at = &spec[..at_pos];
            let digest = &spec[at_pos + 1..];

            if !digest.starts_with("sha256:") {
                return Err(ImageResolveError::InvalidSpec {
                    spec: spec.to_string(),
                    reason: "digest must start with sha256:".to_string(),
                });
            }

            let (registry, name) = self.split_registry_name(before_at)?;
            return Ok(ImageSpec {
                registry,
                name,
                reference: digest.to_string(),
                is_digest: true,
            });
        }

        // Tag reference — split on last ':'
        let (name_part, tag) = if let Some(colon_pos) = spec.rfind(':') {
            // Make sure the colon is not inside the registry (e.g., "localhost:5000/name")
            let after_colon = &spec[colon_pos + 1..];
            let before_colon = &spec[..colon_pos];

            // If after_colon contains '/' it's part of a path, not a tag
            if after_colon.contains('/') {
                (spec, "latest")
            } else {
                (before_colon, after_colon)
            }
        } else {
            (spec, "latest")
        };

        if tag.is_empty() {
            return Err(ImageResolveError::InvalidSpec {
                spec: spec.to_string(),
                reason: "empty tag".to_string(),
            });
        }

        let (registry, name) = self.split_registry_name(name_part)?;

        Ok(ImageSpec {
            registry,
            name,
            reference: tag.to_string(),
            is_digest: false,
        })
    }

    /// Split a name portion (without tag/digest) into registry and image name.
    ///
    /// A component is considered a registry if it contains a '.' or ':' (port),
    /// otherwise the default registry is used.
    fn split_registry_name(&self, input: &str) -> Result<(String, String), ImageResolveError> {
        if let Some(slash_pos) = input.find('/') {
            let first = &input[..slash_pos];
            let rest = &input[slash_pos + 1..];

            // Heuristic: if the first component has a dot or colon, it's a registry.
            if first.contains('.') || first.contains(':') {
                if rest.is_empty() {
                    return Err(ImageResolveError::InvalidSpec {
                        spec: input.to_string(),
                        reason: "empty image name".to_string(),
                    });
                }
                return Ok((first.to_string(), rest.to_string()));
            }

            // Otherwise treat the whole thing as the name under the default registry.
            Ok((self.default_registry.clone(), input.to_string()))
        } else {
            // No slash at all — just a name.
            Ok((self.default_registry.clone(), input.to_string()))
        }
    }

    /// Build the manifest URL for the given spec.
    fn manifest_url(registry: &str, name: &str, reference: &str) -> String {
        // Ensure registry has a scheme.
        let base = if registry.starts_with("http://") || registry.starts_with("https://") {
            registry.to_string()
        } else {
            format!("https://{registry}")
        };
        format!("{base}/v2/{name}/manifests/{reference}")
    }
}

#[async_trait]
impl ImageResolver for OciRegistryResolver {
    async fn resolve(
        &self,
        spec: &str,
        image_type: ImageType,
    ) -> Result<Option<ImageRef>, ImageResolveError> {
        let parsed = self.parse_spec(spec)?;

        let url = Self::manifest_url(&parsed.registry, &parsed.name, &parsed.reference);

        let response = self
            .client
            .get(&url)
            .header("Accept", OCI_ACCEPT_HEADER)
            .send()
            .await
            .map_err(|e| ImageResolveError::RegistryUnavailable {
                url: url.clone(),
                reason: e.to_string(),
            })?;

        match response.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            401 | 403 => {
                return Err(ImageResolveError::RegistryUnavailable {
                    url,
                    reason: "unauthorized".to_string(),
                });
            }
            status => {
                return Err(ImageResolveError::RegistryUnavailable {
                    url,
                    reason: format!("unexpected status {status}"),
                });
            }
        }

        // Extract digest from Docker-Content-Digest header.
        let digest = response
            .headers()
            .get("Docker-Content-Digest")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let sha256 = digest
            .strip_prefix("sha256:")
            .unwrap_or(&digest)
            .to_string();

        let content_length = response
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(Some(ImageRef {
            spec: spec.to_string(),
            image_type,
            registry: parsed.registry,
            name: parsed.name.clone(),
            version: parsed.reference.clone(),
            original_tag: if parsed.is_digest {
                String::new()
            } else {
                parsed.reference
            },
            sha256,
            size_bytes: content_length,
            mount_point: String::new(),
            resolve_on_schedule: false,
        }))
    }

    async fn metadata(&self, image: &ImageRef) -> Result<ImageMetadata, ImageResolveError> {
        // For OCI images, views come from EDF, not env.json.
        // For uenv images, reading env.json requires mounting the SquashFS,
        // which is done by the node agent or a sidecar. We return empty
        // metadata here; callers should use uenv_metadata::parse_uenv_metadata
        // when they have the env.json content.
        Ok(ImageMetadata {
            name: image.name.clone(),
            description: String::new(),
            mount_point: String::new(),
            views: Vec::new(),
            default_view: None,
        })
    }
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn resolver(default_registry: &str) -> OciRegistryResolver {
        OciRegistryResolver::new(default_registry)
    }

    // ── parse_spec tests ──

    #[test]
    fn parse_spec_full_registry() {
        let r = resolver("default.io");
        let s = r.parse_spec("registry.io/myimage:v1").unwrap();
        assert_eq!(s.registry, "registry.io");
        assert_eq!(s.name, "myimage");
        assert_eq!(s.reference, "v1");
        assert!(!s.is_digest);
    }

    #[test]
    fn parse_spec_with_org() {
        let r = resolver("default.io");
        let s = r.parse_spec("registry.io/org/myimage:v2").unwrap();
        assert_eq!(s.registry, "registry.io");
        assert_eq!(s.name, "org/myimage");
        assert_eq!(s.reference, "v2");
    }

    #[test]
    fn parse_spec_default_registry() {
        let r = resolver("default.io");
        let s = r.parse_spec("myimage:latest").unwrap();
        assert_eq!(s.registry, "default.io");
        assert_eq!(s.name, "myimage");
        assert_eq!(s.reference, "latest");
    }

    #[test]
    fn parse_spec_no_tag_defaults_to_latest() {
        let r = resolver("default.io");
        let s = r.parse_spec("myimage").unwrap();
        assert_eq!(s.reference, "latest");
    }

    #[test]
    fn parse_spec_digest_reference() {
        let r = resolver("default.io");
        let s = r.parse_spec("myimage@sha256:abcdef1234567890").unwrap();
        assert_eq!(s.registry, "default.io");
        assert_eq!(s.name, "myimage");
        assert_eq!(s.reference, "sha256:abcdef1234567890");
        assert!(s.is_digest);
    }

    #[test]
    fn parse_spec_registry_with_digest() {
        let r = resolver("default.io");
        let s = r.parse_spec("registry.io/name@sha256:abc123").unwrap();
        assert_eq!(s.registry, "registry.io");
        assert_eq!(s.name, "name");
        assert_eq!(s.reference, "sha256:abc123");
        assert!(s.is_digest);
    }

    #[test]
    fn parse_spec_empty_errors() {
        let r = resolver("default.io");
        assert!(r.parse_spec("").is_err());
    }

    #[test]
    fn parse_spec_registry_with_port() {
        let r = resolver("default.io");
        let s = r.parse_spec("localhost:5000/myimage:v1").unwrap();
        assert_eq!(s.registry, "localhost:5000");
        assert_eq!(s.name, "myimage");
        assert_eq!(s.reference, "v1");
    }

    // ── resolve integration tests with wiremock ──

    #[tokio::test]
    async fn resolve_returns_image_ref_on_200() {
        let server = MockServer::start().await;

        // Wiremock sets Content-Length from body size, so we provide a body
        // to get a meaningful size_bytes value.
        let body = "x".repeat(4096);
        Mock::given(method("GET"))
            .and(path("/v2/myimage/manifests/v1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Docker-Content-Digest", "sha256:deadbeef")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let uri = server.uri();
        let r = OciRegistryResolver::with_client(reqwest::Client::new(), &uri);

        let result = r.resolve("myimage:v1", ImageType::Oci).await.unwrap();
        let img = result.expect("should resolve");
        assert_eq!(img.sha256, "deadbeef");
        assert_eq!(img.size_bytes, 4096);
        assert_eq!(img.name, "myimage");
        assert_eq!(img.original_tag, "v1");
    }

    #[tokio::test]
    async fn resolve_returns_none_on_404() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v2/missing/manifests/latest"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let r = OciRegistryResolver::with_client(reqwest::Client::new(), &server.uri());

        let result = r.resolve("missing", ImageType::Oci).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_returns_error_on_401() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v2/private/manifests/latest"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let r = OciRegistryResolver::with_client(reqwest::Client::new(), &server.uri());

        let result = r.resolve("private", ImageType::Oci).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ImageResolveError::RegistryUnavailable { .. }),
            "expected RegistryUnavailable, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn resolve_network_error() {
        // Connect to a port that's not listening.
        let r = OciRegistryResolver::with_client(
            reqwest::Client::builder()
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap(),
            "http://127.0.0.1:1",
        );

        let result = r.resolve("anything:v1", ImageType::Uenv).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn metadata_returns_empty_for_oci() {
        let r = resolver("default.io");
        let img = ImageRef {
            name: "test".to_string(),
            ..Default::default()
        };
        let meta = r.metadata(&img).await.unwrap();
        assert!(meta.views.is_empty());
        assert_eq!(meta.name, "test");
    }
}
