//! Bearer token interceptor for gRPC requests.

use tonic::service::Interceptor;

/// Interceptor that adds a bearer token to outgoing gRPC requests.
#[derive(Clone)]
pub struct AuthInterceptor {
    token: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}

impl AuthInterceptor {
    /// Create a new interceptor with an optional bearer token.
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token.and_then(|t| {
                format!("Bearer {t}")
                    .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                    .ok()
            }),
        }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(ref token) = self.token {
            request
                .metadata_mut()
                .insert("authorization", token.clone());
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_token_adds_authorization_header() {
        let mut interceptor = AuthInterceptor::new(Some("test-token-123".to_string()));
        let request = tonic::Request::new(());
        let result = interceptor.call(request).unwrap();
        let auth_value = result.metadata().get("authorization").unwrap();
        assert_eq!(auth_value.to_str().unwrap(), "Bearer test-token-123");
    }

    #[test]
    fn without_token_passes_through() {
        let mut interceptor = AuthInterceptor::new(None);
        let request = tonic::Request::new(());
        let result = interceptor.call(request).unwrap();
        assert!(result.metadata().get("authorization").is_none());
    }
}
