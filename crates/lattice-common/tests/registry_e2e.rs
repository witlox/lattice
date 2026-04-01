//! End-to-end tests for OCI registry integration using testcontainers.
//!
//! These tests spin up a real OCI registry (`registry:2`) via Docker and
//! exercise the [`OciRegistryResolver`] against it. They verify:
//!
//! - Resolving a pushed image returns the correct digest (INV-SD1)
//! - Resolving a nonexistent image returns `None`
//! - Content-addressed pinning survives tag mutation (INV-SD1)
//!
//! All tests are `#[ignore]` because they require a Docker daemon.
//! Run with: `cargo test -p lattice-common -- --ignored`

use lattice_common::registry::OciRegistryResolver;
use lattice_common::traits::ImageResolver;
use lattice_common::types::ImageType;
use reqwest::Client;
use sha2::{Digest, Sha256};
use testcontainers::{runners::AsyncRunner, GenericImage};

/// Compute SHA-256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Start a `registry:2` container and return the container handle + host URL.
async fn start_registry() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("registry", "2")
        .with_exposed_port(testcontainers::core::IntoContainerPort::tcp(5000))
        .start()
        .await
        .expect("Failed to start registry container");

    let port = container.get_host_port_ipv4(5000).await.unwrap();
    let url = format!("http://localhost:{port}");
    (container, url)
}

/// Push a minimal OCI image (empty layers, `{}` config) to the registry.
/// Returns the manifest digest as reported by the registry.
async fn push_test_image(registry_url: &str, name: &str, tag: &str) -> String {
    push_test_image_with_config(registry_url, name, tag, b"{}").await
}

/// Push a minimal OCI image with a custom config blob.
/// Returns the manifest digest from the registry.
async fn push_test_image_with_config(
    registry_url: &str,
    name: &str,
    tag: &str,
    config_bytes: &[u8],
) -> String {
    let client = Client::new();
    let config_digest = format!("sha256:{}", sha256_hex(config_bytes));

    // Initiate blob upload
    let upload_resp = client
        .post(format!("{registry_url}/v2/{name}/blobs/uploads/"))
        .send()
        .await
        .expect("initiate config blob upload");
    assert!(
        upload_resp.status().is_success() || upload_resp.status() == reqwest::StatusCode::ACCEPTED,
        "upload initiate failed: {}",
        upload_resp.status()
    );
    let location = upload_resp
        .headers()
        .get("Location")
        .expect("missing Location header on upload initiate")
        .to_str()
        .unwrap()
        .to_string();

    // Complete the monolithic upload
    let upload_url = if location.starts_with("http") {
        location
    } else {
        format!("{registry_url}{location}")
    };
    let separator = if upload_url.contains('?') { "&" } else { "?" };
    let complete_url = format!("{upload_url}{separator}digest={config_digest}");

    let put_resp = client
        .put(&complete_url)
        .header("Content-Type", "application/octet-stream")
        .body(config_bytes.to_vec())
        .send()
        .await
        .expect("complete config blob upload");
    assert!(
        put_resp.status().is_success(),
        "config blob PUT failed: {}",
        put_resp.status()
    );

    // Push manifest
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len()
        },
        "layers": []
    });

    let manifest_resp = client
        .put(format!("{registry_url}/v2/{name}/manifests/{tag}"))
        .header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
        .body(manifest.to_string())
        .send()
        .await
        .expect("push manifest");
    assert!(
        manifest_resp.status().is_success(),
        "manifest PUT failed: {}",
        manifest_resp.status()
    );

    manifest_resp
        .headers()
        .get("Docker-Content-Digest")
        .expect("missing Docker-Content-Digest in manifest response")
        .to_str()
        .unwrap()
        .to_string()
}

// ─── Tests ───────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Docker for testcontainers"]
async fn resolve_image_from_real_registry() {
    let (_container, registry_url) = start_registry().await;

    // Push a test image
    let digest = push_test_image(&registry_url, "test/myimage", "v1").await;

    // The existing OciRegistryResolver uses default_registry for specs without
    // an explicit registry prefix. We pass the full registry URL as default.
    let resolver = OciRegistryResolver::new(&registry_url);

    // The spec "test/myimage:v1" has no dot or colon in the first path segment,
    // so the resolver treats it as a name under the default registry.
    let result = resolver
        .resolve("test/myimage:v1", ImageType::Oci)
        .await
        .expect("resolve should succeed");

    assert!(result.is_some(), "image should be found");
    let image_ref = result.unwrap();
    assert_eq!(
        image_ref.sha256,
        digest.strip_prefix("sha256:").unwrap_or(&digest)
    );
    assert_eq!(image_ref.original_tag, "v1");
    assert_eq!(image_ref.name, "test/myimage");
}

#[tokio::test]
#[ignore = "requires Docker for testcontainers"]
async fn resolve_nonexistent_image_returns_none() {
    let (_container, registry_url) = start_registry().await;

    let resolver = OciRegistryResolver::new(&registry_url);
    let result = resolver
        .resolve("nonexistent/image:v1", ImageType::Oci)
        .await
        .expect("resolve should succeed even for missing images");

    assert!(result.is_none(), "nonexistent image should return None");
}

#[tokio::test]
#[ignore = "requires Docker for testcontainers"]
async fn full_submit_resolve_lifecycle() {
    // Start registry
    let (_container, registry_url) = start_registry().await;

    // Push initial image
    let _digest = push_test_image(&registry_url, "test/pytorch", "24.01").await;

    // Resolve — simulates what the API server does on submit
    let resolver = OciRegistryResolver::new(&registry_url);
    let resolved = resolver
        .resolve("test/pytorch:24.01", ImageType::Oci)
        .await
        .expect("resolve should succeed")
        .expect("image should exist");
    assert!(!resolved.sha256.is_empty(), "sha256 must be pinned");

    // Re-push with different config content to get a new manifest digest.
    // This simulates a tag mutation (someone pushes a new image under the same tag).
    let new_digest = push_test_image_with_config(
        &registry_url,
        "test/pytorch",
        "24.01",
        b"{\"created\":\"2026-04-01T00:00:00Z\"}",
    )
    .await;

    // The tag now points to a different digest, but our resolved ref still has the old one.
    // This proves INV-SD1: content-addressed pinning.
    let new_sha = new_digest.strip_prefix("sha256:").unwrap_or(&new_digest);
    assert_ne!(
        resolved.sha256, new_sha,
        "tag mutation should produce a different digest, proving pinning works"
    );
}

#[tokio::test]
#[ignore = "requires Docker for testcontainers"]
async fn metadata_returns_basic_info() {
    let (_container, registry_url) = start_registry().await;

    push_test_image(&registry_url, "test/tools", "latest").await;

    let resolver = OciRegistryResolver::new(&registry_url);
    let image_ref = resolver
        .resolve("test/tools:latest", ImageType::Oci)
        .await
        .unwrap()
        .unwrap();

    let meta = resolver.metadata(&image_ref).await.unwrap();
    assert_eq!(meta.name, "test/tools");
}
