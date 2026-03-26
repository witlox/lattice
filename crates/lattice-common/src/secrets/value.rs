use std::fmt;
use zeroize::Zeroize;

/// Opaque wrapper for resolved secret values.
///
/// The only way to access the inner value is via `expose()`.
/// `Debug` and `Display` always emit `[REDACTED]` (INV-SEC2).
/// Memory is zeroized on drop (SEC-F4).
#[derive(Clone)]
pub struct SecretValue {
    inner: String,
}

impl SecretValue {
    /// Reveal the secret value. Use only when passing to client constructors.
    pub fn expose(&self) -> &str {
        &self.inner
    }
}

impl From<String> for SecretValue {
    fn from(s: String) -> Self {
        Self { inner: s }
    }
}

impl From<&str> for SecretValue {
    fn from(s: &str) -> Self {
        Self {
            inner: s.to_string(),
        }
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expose_reveals_value() {
        let sv = SecretValue::from("my-secret");
        assert_eq!(sv.expose(), "my-secret");
    }

    #[test]
    fn debug_redacts_value() {
        let sv = SecretValue::from("my-secret");
        let debug = format!("{sv:?}");
        assert!(!debug.contains("my-secret"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn display_redacts_value() {
        let sv = SecretValue::from("my-secret");
        let display = format!("{sv}");
        assert!(!display.contains("my-secret"));
        assert!(display.contains("REDACTED"));
    }

    #[test]
    fn clone_produces_independent_copy() {
        let sv = SecretValue::from("my-secret");
        let cloned = sv.clone();
        assert_eq!(cloned.expose(), "my-secret");
    }
}
