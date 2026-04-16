//! # Cert-SAN binding middleware (INV-D14)
//!
//! Validates that an agent's `agent_address` appears as a Subject Alternative
//! Name (SAN) in the peer workload certificate presented during the mTLS
//! handshake. Enforced at `RegisterNode` and `UpdateNodeAddress` handler
//! entry, with a dev-mode AllowAll pathway for deployments that have not
//! configured mTLS yet (e.g., OV docker-compose).
//!
//! ## Integration seam
//!
//! Actual extraction of peer-cert SANs from the tonic `TcpConnectInfo` ⇄
//! rustls session is a TLS-layer concern. This module defines the
//! `SanValidator` trait + two implementations (dev AllowAll, strict Bound).
//! Handlers call `validator.validate(&addr, &request.extensions())` — in
//! production the tonic connection extractor populates a `PeerCertSans`
//! extension; in dev mode the extension is absent and AllowAll accepts.
//!
//! All validation logic below is unit-tested; the extension-population
//! integration sits behind `extract_peer_sans_from_request()` which reads
//! a `PeerCertSans` request extension (implementer of TLS wiring fills
//! this at connection time).

use std::fmt;
use std::sync::Arc;

/// Subject Alternative Names extracted from the peer certificate of an
/// mTLS connection. Attached as a tonic request extension by the TLS
/// connection extractor; read by SanValidator implementations.
#[derive(Debug, Clone, Default)]
pub struct PeerCertSans {
    /// DNS and IP SANs from the workload certificate. Both kinds are
    /// kept in the same list because the matching rule is
    /// "host portion of agent_address ∈ this set" regardless of kind.
    pub sans: Vec<String>,
}

impl PeerCertSans {
    pub fn new(sans: Vec<String>) -> Self {
        Self { sans }
    }
    pub fn is_empty(&self) -> bool {
        self.sans.is_empty()
    }
}

/// Trait that decides whether a proposed `agent_address` is permitted
/// given the caller's peer certificate SANs.
pub trait SanValidator: Send + Sync + fmt::Debug {
    /// Accept or reject. Err message is surfaced via the corresponding
    /// Response's `reason` field.
    fn validate(&self, agent_address: &str, peer_sans: Option<&PeerCertSans>)
        -> Result<(), String>;
}

/// Dev-mode validator: accepts any address. Used when mTLS is not
/// configured (docker-compose, unit tests, single-node dev). Production
/// deployments MUST swap in `BoundToPeerCertSanValidator` (typically
/// constructed from an mTLS-configured ApiState).
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllSanValidator;

impl SanValidator for AllowAllSanValidator {
    fn validate(
        &self,
        _agent_address: &str,
        _peer_sans: Option<&PeerCertSans>,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Production validator: requires that `agent_address`'s host appears in
/// the peer certificate SANs. Rejects when peer SANs are absent (unless
/// `dev_bypass` is true, which is intended only for bootstrap scenarios).
#[derive(Debug, Clone)]
pub struct BoundToPeerCertSanValidator {
    /// When true, requests arriving without `PeerCertSans` attached
    /// (i.e., no mTLS on the connection) are accepted. Matches the
    /// `oidc_issuer == ""` dev-mode gate used elsewhere in the API.
    pub dev_bypass_when_missing: bool,
}

impl SanValidator for BoundToPeerCertSanValidator {
    fn validate(
        &self,
        agent_address: &str,
        peer_sans: Option<&PeerCertSans>,
    ) -> Result<(), String> {
        let Some(sans) = peer_sans else {
            if self.dev_bypass_when_missing {
                return Ok(());
            }
            return Err("mtls_required_but_no_peer_certs".into());
        };
        if sans.is_empty() {
            return Err("mtls_peer_cert_has_no_sans".into());
        }
        let host = extract_host(agent_address);
        for san in &sans.sans {
            if san_matches_host(san, host) {
                return Ok(());
            }
        }
        Err(format!(
            "address_not_in_cert_sans: host={host}, sans={sans:?}",
            sans = sans.sans,
        ))
    }
}

/// Parse the host portion out of a "host:port" string. Treats bracketed
/// IPv6 literals correctly ([::1]:50052 → ::1).
fn extract_host(addr: &str) -> &str {
    let trimmed = addr.trim();
    // IPv6 literal: [host]:port
    if let Some(bracket_end) = trimmed.find(']') {
        return trimmed[1..bracket_end].trim_start_matches('[');
    }
    // host:port
    if let Some(idx) = trimmed.rfind(':') {
        return &trimmed[..idx];
    }
    trimmed
}

/// SAN matching rule. Supports exact match and wildcard DNS names
/// (`*.example.com` matches `agent.example.com` but not `x.y.example.com`).
fn san_matches_host(san: &str, host: &str) -> bool {
    if san.eq_ignore_ascii_case(host) {
        return true;
    }
    if let Some(suffix) = san.strip_prefix("*.") {
        if let Some(dot_idx) = host.find('.') {
            let host_suffix = &host[dot_idx + 1..];
            if host_suffix.eq_ignore_ascii_case(suffix) {
                return true;
            }
        }
    }
    false
}

/// Convenience Arc alias for storage in ApiState.
pub type SharedSanValidator = Arc<dyn SanValidator>;

/// Default validator for dev mode — AllowAll wrapped as Arc.
pub fn default_dev_validator() -> SharedSanValidator {
    Arc::new(AllowAllSanValidator)
}

/// Build a strict validator. When `dev_bypass_when_missing` is true,
/// requests arriving without peer certs (no mTLS on the connection)
/// still pass; operators can turn this off once mTLS is mandatory.
pub fn bound_validator(dev_bypass_when_missing: bool) -> SharedSanValidator {
    Arc::new(BoundToPeerCertSanValidator {
        dev_bypass_when_missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_all_accepts_anything() {
        let v = AllowAllSanValidator;
        assert!(v.validate("10.0.0.1:50052", None).is_ok());
        assert!(v
            .validate("10.0.0.1:50052", Some(&PeerCertSans::new(vec![])))
            .is_ok());
    }

    #[test]
    fn bound_validator_accepts_exact_match() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let sans = PeerCertSans::new(vec!["10.1.0.5".into(), "agent1.hsn.example".into()]);
        assert!(v.validate("10.1.0.5:50052", Some(&sans)).is_ok());
        assert!(v.validate("agent1.hsn.example:50052", Some(&sans)).is_ok());
    }

    #[test]
    fn bound_validator_rejects_address_not_in_sans() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let sans = PeerCertSans::new(vec!["10.1.0.5".into()]);
        let err = v.validate("10.1.0.99:50052", Some(&sans)).unwrap_err();
        assert!(err.contains("address_not_in_cert_sans"), "got: {err}");
    }

    #[test]
    fn bound_validator_accepts_wildcard_san() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let sans = PeerCertSans::new(vec!["*.hsn.example".into()]);
        assert!(v
            .validate("agent-42.hsn.example:50052", Some(&sans))
            .is_ok());
        // Wildcard does NOT match multiple labels.
        assert!(v.validate("a.b.hsn.example:50052", Some(&sans)).is_err());
    }

    #[test]
    fn bound_validator_dev_bypass_accepts_missing_peer_certs() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: true,
        };
        assert!(v.validate("10.0.0.1:50052", None).is_ok());
    }

    #[test]
    fn bound_validator_strict_rejects_missing_peer_certs() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let err = v.validate("10.0.0.1:50052", None).unwrap_err();
        assert!(err.contains("mtls_required_but_no_peer_certs"));
    }

    #[test]
    fn extract_host_handles_ipv6() {
        assert_eq!(extract_host("[::1]:50052"), "::1");
        assert_eq!(extract_host("[fe80::1]:9000"), "fe80::1");
    }

    #[test]
    fn extract_host_handles_ipv4_and_dns() {
        assert_eq!(extract_host("10.1.0.5:50052"), "10.1.0.5");
        assert_eq!(extract_host("agent.example:50052"), "agent.example");
        assert_eq!(extract_host("localhost:8080"), "localhost");
    }

    #[test]
    fn empty_sans_list_always_fails() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let sans = PeerCertSans::new(vec![]);
        let err = v.validate("10.0.0.1:50052", Some(&sans)).unwrap_err();
        assert!(err.contains("mtls_peer_cert_has_no_sans"));
    }

    #[test]
    fn case_insensitive_dns_match() {
        let v = BoundToPeerCertSanValidator {
            dev_bypass_when_missing: false,
        };
        let sans = PeerCertSans::new(vec!["Agent1.HSN.Example".into()]);
        assert!(v.validate("agent1.hsn.example:50052", Some(&sans)).is_ok());
    }
}
