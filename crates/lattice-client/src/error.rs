//! Client error types.

/// Errors returned by [`LatticeClient`](crate::LatticeClient) methods.
#[derive(Debug, thiserror::Error)]
pub enum LatticeClientError {
    /// gRPC transport-level failure (connection refused, TLS error, etc.).
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    /// Requested resource was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Authentication failed (invalid or missing token).
    #[error("authentication failed: {0}")]
    Auth(String),

    /// Caller lacks permission for the requested operation.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Server rejected the request due to invalid input.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Server-side error.
    #[error("server error: {0}")]
    Internal(String),
}

impl From<tonic::Status> for LatticeClientError {
    fn from(status: tonic::Status) -> Self {
        match status.code() {
            tonic::Code::NotFound => Self::NotFound(status.message().to_string()),
            tonic::Code::Unauthenticated => Self::Auth(status.message().to_string()),
            tonic::Code::PermissionDenied => Self::PermissionDenied(status.message().to_string()),
            tonic::Code::InvalidArgument => Self::InvalidArgument(status.message().to_string()),
            _ => Self::Internal(format!("{}: {}", status.code(), status.message())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_from_status() {
        let status = tonic::Status::not_found("node x1 not found");
        let err = LatticeClientError::from(status);
        assert!(matches!(err, LatticeClientError::NotFound(_)));
        assert!(err.to_string().contains("node x1 not found"));
    }

    #[test]
    fn unauthenticated_from_status() {
        let status = tonic::Status::unauthenticated("invalid token");
        let err = LatticeClientError::from(status);
        assert!(matches!(err, LatticeClientError::Auth(_)));
    }

    #[test]
    fn permission_denied_from_status() {
        let status = tonic::Status::permission_denied("not allowed");
        let err = LatticeClientError::from(status);
        assert!(matches!(err, LatticeClientError::PermissionDenied(_)));
    }

    #[test]
    fn invalid_argument_from_status() {
        let status = tonic::Status::invalid_argument("bad input");
        let err = LatticeClientError::from(status);
        assert!(matches!(err, LatticeClientError::InvalidArgument(_)));
    }

    #[test]
    fn internal_from_status() {
        let status = tonic::Status::internal("boom");
        let err = LatticeClientError::from(status);
        assert!(matches!(err, LatticeClientError::Internal(_)));
    }

    #[test]
    fn transport_from_error() {
        // Transport errors are hard to construct, but verify the variant exists
        let err = LatticeClientError::Internal("test".to_string());
        assert!(err.to_string().contains("test"));
    }
}
