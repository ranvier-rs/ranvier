use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Defines types that contain sensitive PII or PHI data and should be redacted in logs or regular outputs.
pub trait Redact {
    fn redact(&self) -> String;
}

/// A wrapper for sensitive data indicating it falls under GDPR or HIPAA compliance scope.
/// The inner data is strictly prevented from being debug-printed or logged by default.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sensitive<T>(T);

impl<T> Sensitive<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Explicitly unwrap and access the sensitive data. Use with caution.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

// Redact by default in Debug
impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

// Redact by default in Display
impl<T> fmt::Display for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

// Implement standard transparent Serialization (assume transmission over TLS is secure)
impl<T: Serialize> Serialize for Sensitive<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Sensitive<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Sensitive)
    }
}

pub trait PiiDetector {
    /// Detects if the given text likely contains Personally Identifiable Information
    fn contains_pii(&self, text: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensitive_redaction() {
        let email = Sensitive::new("user@example.com".to_string());

        // Debug and Display must be redacted
        assert_eq!(format!("{:?}", email), "[REDACTED]");
        assert_eq!(format!("{}", email), "[REDACTED]");

        // Access requires explicit expose
        assert_eq!(email.expose(), "user@example.com");
    }

    #[test]
    fn test_sensitive_serialization() {
        let password = Sensitive::new("my_secret_pass".to_string());

        let json = serde_json::to_string(&password).unwrap();
        // Serialized text transmits the real value
        assert_eq!(json, "\"my_secret_pass\"");

        let deserialized: Sensitive<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.expose(), "my_secret_pass");
    }
}
