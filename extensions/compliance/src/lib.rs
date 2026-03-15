use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

// ---------------------------------------------------------------------------
// ClassificationLevel
// ---------------------------------------------------------------------------

/// Data classification level for compliance policies.
///
/// Higher levels require more stringent access controls and handling procedures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClassificationLevel {
    /// Freely shareable data.
    Public,
    /// Organization-internal data, not for public distribution.
    Internal,
    /// Sensitive data requiring access controls (e.g., financial records).
    Confidential,
    /// Highly sensitive data (e.g., PII, PHI, credentials). Requires explicit grant.
    Restricted,
}

impl fmt::Display for ClassificationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClassificationLevel::Public => write!(f, "Public"),
            ClassificationLevel::Internal => write!(f, "Internal"),
            ClassificationLevel::Confidential => write!(f, "Confidential"),
            ClassificationLevel::Restricted => write!(f, "Restricted"),
        }
    }
}

// ---------------------------------------------------------------------------
// Redact trait
// ---------------------------------------------------------------------------

/// Defines types that contain sensitive PII or PHI data and should be redacted in logs or regular outputs.
pub trait Redact {
    fn redact(&self) -> String;
}

// ---------------------------------------------------------------------------
// Sensitive<T>
// ---------------------------------------------------------------------------

/// A wrapper for sensitive data indicating it falls under GDPR or HIPAA compliance scope.
/// The inner data is strictly prevented from being debug-printed or logged by default.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sensitive<T> {
    value: T,
    /// Data classification level. Defaults to `Restricted`.
    pub classification: ClassificationLevel,
}

impl<T> Sensitive<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            classification: ClassificationLevel::Restricted,
        }
    }

    /// Create a Sensitive wrapper with a specific classification level.
    pub fn with_classification(value: T, classification: ClassificationLevel) -> Self {
        Self {
            value,
            classification,
        }
    }

    /// Explicitly unwrap and access the sensitive data. Use with caution.
    pub fn expose(&self) -> &T {
        &self.value
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}

// Redact by default in Debug
impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED:{}]", self.classification)
    }
}

// Redact by default in Display
impl<T> fmt::Display for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

/// Serialization for `Sensitive<T>`.
///
/// In **debug builds** (`cfg(debug_assertions)`), the inner value is serialized
/// transparently so that local development and testing work as expected.
///
/// In **release builds**, the value is replaced with the string `"[REDACTED]"`
/// to prevent accidental PII/PHI leakage into logs, API responses, or external
/// systems. Use [`Sensitive::expose()`] to access the underlying value when
/// explicit transmission is required.
impl<T: Serialize> Serialize for Sensitive<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if cfg!(debug_assertions) {
            self.value.serialize(serializer)
        } else {
            serializer.serialize_str("[REDACTED]")
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Sensitive<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Sensitive::new)
    }
}

// ---------------------------------------------------------------------------
// EncryptionHook
// ---------------------------------------------------------------------------

/// Hook for field-level encryption/decryption of sensitive data.
pub trait EncryptionHook: Send + Sync {
    /// Encrypt the given plaintext bytes.
    fn encrypt(&self, data: &[u8]) -> Vec<u8>;

    /// Decrypt the given ciphertext bytes.
    fn decrypt(&self, data: &[u8]) -> Vec<u8>;
}

/// No-op encryption hook that passes data through unchanged.
pub struct NoOpEncryption;

impl EncryptionHook for NoOpEncryption {
    fn encrypt(&self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }

    fn decrypt(&self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }
}

/// XOR-based encryption hook for testing/demo purposes.
///
/// # Security Warning
///
/// **NOT SUITABLE FOR PRODUCTION.** This uses a single-byte key (256 possible
/// values) and is trivially breakable via brute force. Use AES-GCM,
/// ChaCha20Poly1305, or similar authenticated encryption for real workloads.
///
/// This struct is gated behind the `xor-demo` feature to prevent accidental
/// use in production builds.
#[cfg(feature = "xor-demo")]
#[deprecated(
    since = "0.32.0",
    note = "XOR encryption is cryptographically broken. Use AES-GCM or ChaCha20Poly1305 for production."
)]
pub struct XorEncryption {
    key: u8,
}

#[cfg(feature = "xor-demo")]
#[allow(deprecated)]
impl XorEncryption {
    pub fn new(key: u8) -> Self {
        Self { key }
    }
}

#[cfg(feature = "xor-demo")]
#[allow(deprecated)]
impl EncryptionHook for XorEncryption {
    fn encrypt(&self, data: &[u8]) -> Vec<u8> {
        data.iter().map(|b| b ^ self.key).collect()
    }

    fn decrypt(&self, data: &[u8]) -> Vec<u8> {
        // XOR is its own inverse
        self.encrypt(data)
    }
}

// ---------------------------------------------------------------------------
// PII Detection
// ---------------------------------------------------------------------------

/// A detected PII field with its classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiiField {
    /// The JSON path or field name that was flagged.
    pub field_name: String,
    /// The suggested classification level.
    pub classification: ClassificationLevel,
    /// The PII category that triggered the match.
    pub category: String,
}

/// Detects PII in field names using pattern matching.
pub struct FieldNamePiiDetector {
    patterns: Vec<(Vec<&'static str>, &'static str, ClassificationLevel)>,
}

impl Default for FieldNamePiiDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl FieldNamePiiDetector {
    pub fn new() -> Self {
        Self {
            patterns: vec![
                (
                    vec!["email", "e_mail", "email_address"],
                    "email",
                    ClassificationLevel::Confidential,
                ),
                (
                    vec!["phone", "phone_number", "mobile", "tel", "telephone"],
                    "phone",
                    ClassificationLevel::Confidential,
                ),
                (
                    vec!["ssn", "social_security", "social_security_number"],
                    "ssn",
                    ClassificationLevel::Restricted,
                ),
                (
                    vec![
                        "address", "street", "street_address", "home_address",
                        "postal_code", "zip_code", "zip",
                    ],
                    "address",
                    ClassificationLevel::Confidential,
                ),
                (
                    vec![
                        "first_name", "last_name", "full_name", "given_name",
                        "family_name", "surname",
                    ],
                    "name",
                    ClassificationLevel::Confidential,
                ),
                (
                    vec!["ip", "ip_address", "ipv4", "ipv6", "client_ip", "remote_addr"],
                    "ip_address",
                    ClassificationLevel::Internal,
                ),
                (
                    vec![
                        "credit_card", "card_number", "cc_number", "pan",
                        "payment_card",
                    ],
                    "credit_card",
                    ClassificationLevel::Restricted,
                ),
                (
                    vec!["password", "passwd", "secret", "api_key", "access_token"],
                    "credential",
                    ClassificationLevel::Restricted,
                ),
                (
                    vec!["date_of_birth", "dob", "birth_date", "birthday"],
                    "date_of_birth",
                    ClassificationLevel::Confidential,
                ),
                // Korean PII patterns
                (
                    vec!["jumin", "jumin_number", "resident_number", "resident_registration"],
                    "kr_resident_number",
                    ClassificationLevel::Restricted,
                ),
                (
                    vec!["business_number", "saeopja", "business_registration"],
                    "kr_business_number",
                    ClassificationLevel::Confidential,
                ),
                (
                    vec!["passport", "passport_number", "yeokkwon"],
                    "passport",
                    ClassificationLevel::Restricted,
                ),
                (
                    vec!["drivers_license", "driver_license", "license_number", "myeonheo"],
                    "drivers_license",
                    ClassificationLevel::Restricted,
                ),
            ],
        }
    }

    /// Classify a field name, returning the classification level if it matches a PII pattern.
    pub fn classify(&self, field_name: &str) -> Option<ClassificationLevel> {
        let lower = field_name.to_lowercase();
        for (patterns, _, level) in &self.patterns {
            if patterns.iter().any(|p| lower == *p || lower.contains(p)) {
                return Some(*level);
            }
        }
        None
    }

    /// Scan a JSON value for PII field names, returning all detected PII fields.
    pub fn scan_value(&self, value: &serde_json::Value) -> Vec<PiiField> {
        let mut results = Vec::new();
        self.scan_recursive(value, "", &mut results);
        results
    }

    fn scan_recursive(
        &self,
        value: &serde_json::Value,
        path: &str,
        results: &mut Vec<PiiField>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let field_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };

                    let lower = key.to_lowercase();
                    for (patterns, category, level) in &self.patterns {
                        if patterns.iter().any(|p| lower == *p || lower.contains(p)) {
                            results.push(PiiField {
                                field_name: field_path.clone(),
                                classification: *level,
                                category: category.to_string(),
                            });
                            break;
                        }
                    }

                    self.scan_recursive(val, &field_path, results);
                }
            }
            serde_json::Value::Array(arr) => {
                for (i, val) in arr.iter().enumerate() {
                    let item_path = format!("{path}[{i}]");
                    self.scan_recursive(val, &item_path, results);
                }
            }
            _ => {}
        }
    }
}

/// Trait for PII detection on text content.
pub trait PiiDetector {
    /// Detects if the given text likely contains Personally Identifiable Information
    fn contains_pii(&self, text: &str) -> bool;
}

impl PiiDetector for FieldNamePiiDetector {
    fn contains_pii(&self, text: &str) -> bool {
        self.classify(text).is_some()
    }
}

// ---------------------------------------------------------------------------
// Right-to-Erasure (GDPR Article 17)
// ---------------------------------------------------------------------------

/// A request to erase personal data for a specific subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErasureRequest {
    /// Unique identifier for this erasure request.
    pub id: String,
    /// The data subject identifier (e.g., user ID, email).
    pub subject: String,
    /// Scope of erasure (e.g., "all", "transactions", specific table names).
    pub scope: Vec<String>,
    /// When the request was made.
    pub timestamp: DateTime<Utc>,
    /// Optional reason for the request.
    pub reason: Option<String>,
}

impl ErasureRequest {
    pub fn new(id: String, subject: String, scope: Vec<String>) -> Self {
        Self {
            id,
            subject,
            scope,
            timestamp: Utc::now(),
            reason: None,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Result of processing an erasure request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErasureResult {
    /// The original request ID.
    pub request_id: String,
    /// Whether the erasure was successfully completed.
    pub success: bool,
    /// Number of records erased.
    pub records_erased: usize,
    /// Any scopes that could not be processed.
    pub failed_scopes: Vec<String>,
    /// When the erasure was completed.
    pub completed_at: DateTime<Utc>,
}

/// Sink for processing data erasure requests.
pub trait ErasureSink: Send + Sync {
    /// Process a data erasure request.
    fn erase(&self, request: &ErasureRequest) -> ErasureResult;
}

/// In-memory erasure sink for testing.
pub struct InMemoryErasureSink {
    records: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
}

impl Default for InMemoryErasureSink {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryErasureSink {
    pub fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Add records for a subject (for testing purposes).
    pub fn add_records(&self, subject: &str, data: Vec<String>) {
        let mut records = self.records.lock().unwrap();
        records.insert(subject.to_string(), data);
    }
}

impl ErasureSink for InMemoryErasureSink {
    fn erase(&self, request: &ErasureRequest) -> ErasureResult {
        let mut records = self.records.lock().unwrap();
        let count = if let Some(data) = records.remove(&request.subject) {
            data.len()
        } else {
            0
        };

        ErasureResult {
            request_id: request.id.clone(),
            success: true,
            records_erased: count,
            failed_scopes: Vec::new(),
            completed_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensitive_redaction() {
        let email = Sensitive::new("user@example.com".to_string());

        assert_eq!(format!("{:?}", email), "[REDACTED:Restricted]");
        assert_eq!(format!("{}", email), "[REDACTED]");
        assert_eq!(email.expose(), "user@example.com");
    }

    #[test]
    fn test_sensitive_with_classification() {
        let data = Sensitive::with_classification("internal doc", ClassificationLevel::Internal);
        assert_eq!(data.classification, ClassificationLevel::Internal);
        assert_eq!(format!("{:?}", data), "[REDACTED:Internal]");
    }

    #[test]
    fn test_sensitive_serialization() {
        let password = Sensitive::new("my_secret_pass".to_string());

        let json = serde_json::to_string(&password).unwrap();
        assert_eq!(json, "\"my_secret_pass\"");

        let deserialized: Sensitive<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.expose(), "my_secret_pass");
    }

    #[test]
    fn classification_level_ordering() {
        assert!(ClassificationLevel::Public < ClassificationLevel::Internal);
        assert!(ClassificationLevel::Internal < ClassificationLevel::Confidential);
        assert!(ClassificationLevel::Confidential < ClassificationLevel::Restricted);
    }

    #[test]
    fn noop_encryption_passthrough() {
        let hook = NoOpEncryption;
        let data = b"hello world";
        let encrypted = hook.encrypt(data);
        let decrypted = hook.decrypt(&encrypted);
        assert_eq!(decrypted, data);
    }

    #[cfg(feature = "xor-demo")]
    #[test]
    #[allow(deprecated)]
    fn xor_encryption_roundtrip() {
        let hook = XorEncryption::new(0x42);
        let data = b"sensitive data";
        let encrypted = hook.encrypt(data);
        assert_ne!(encrypted, data, "Encrypted should differ from plaintext");
        let decrypted = hook.decrypt(&encrypted);
        assert_eq!(decrypted, data, "Decrypted should match original");
    }

    #[test]
    fn pii_detector_classifies_email() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("email"),
            Some(ClassificationLevel::Confidential)
        );
        assert_eq!(
            detector.classify("email_address"),
            Some(ClassificationLevel::Confidential)
        );
    }

    #[test]
    fn pii_detector_classifies_ssn_as_restricted() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("ssn"),
            Some(ClassificationLevel::Restricted)
        );
        assert_eq!(
            detector.classify("social_security_number"),
            Some(ClassificationLevel::Restricted)
        );
    }

    #[test]
    fn pii_detector_no_match_for_regular_fields() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(detector.classify("created_at"), None);
        assert_eq!(detector.classify("status"), None);
        assert_eq!(detector.classify("quantity"), None);
    }

    #[test]
    fn pii_detector_scan_json_value() {
        let detector = FieldNamePiiDetector::new();
        let json = serde_json::json!({
            "id": 1,
            "email": "test@example.com",
            "profile": {
                "first_name": "Alice",
                "phone": "555-1234"
            },
            "status": "active"
        });

        let results = detector.scan_value(&json);
        assert_eq!(results.len(), 3);

        let field_names: Vec<&str> = results.iter().map(|f| f.field_name.as_str()).collect();
        assert!(field_names.contains(&"email"));
        assert!(field_names.contains(&"profile.first_name"));
        assert!(field_names.contains(&"profile.phone"));
    }

    #[test]
    fn pii_detector_trait_impl() {
        let detector = FieldNamePiiDetector::new();
        assert!(detector.contains_pii("email"));
        assert!(!detector.contains_pii("status"));
    }

    #[test]
    fn erasure_request_processing() {
        let sink = InMemoryErasureSink::new();
        sink.add_records(
            "user_42",
            vec![
                "record1".into(),
                "record2".into(),
                "record3".into(),
            ],
        );

        let request = ErasureRequest::new(
            "req_001".into(),
            "user_42".into(),
            vec!["all".into()],
        )
        .with_reason("GDPR Article 17 request");

        let result = sink.erase(&request);
        assert!(result.success);
        assert_eq!(result.records_erased, 3);
        assert!(result.failed_scopes.is_empty());
    }

    #[test]
    fn erasure_request_for_missing_subject() {
        let sink = InMemoryErasureSink::new();
        let request = ErasureRequest::new(
            "req_002".into(),
            "unknown_user".into(),
            vec!["all".into()],
        );

        let result = sink.erase(&request);
        assert!(result.success);
        assert_eq!(result.records_erased, 0);
    }

    // --- New tests for M241 ---

    #[test]
    fn sensitive_display_masking_all_levels() {
        let public = Sensitive::with_classification("data", ClassificationLevel::Public);
        let internal = Sensitive::with_classification("data", ClassificationLevel::Internal);
        let confidential =
            Sensitive::with_classification("data", ClassificationLevel::Confidential);
        let restricted = Sensitive::with_classification("data", ClassificationLevel::Restricted);

        assert_eq!(format!("{}", public), "[REDACTED]");
        assert_eq!(format!("{}", internal), "[REDACTED]");
        assert_eq!(format!("{}", confidential), "[REDACTED]");
        assert_eq!(format!("{}", restricted), "[REDACTED]");
    }

    #[test]
    fn sensitive_debug_includes_level() {
        let public = Sensitive::with_classification("data", ClassificationLevel::Public);
        let restricted = Sensitive::with_classification("data", ClassificationLevel::Restricted);

        assert_eq!(format!("{:?}", public), "[REDACTED:Public]");
        assert_eq!(format!("{:?}", restricted), "[REDACTED:Restricted]");
    }

    #[test]
    fn pii_detector_korean_resident_number() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("jumin_number"),
            Some(ClassificationLevel::Restricted)
        );
        assert_eq!(
            detector.classify("resident_registration"),
            Some(ClassificationLevel::Restricted)
        );
    }

    #[test]
    fn pii_detector_korean_business_number() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("business_number"),
            Some(ClassificationLevel::Confidential)
        );
        assert_eq!(
            detector.classify("business_registration"),
            Some(ClassificationLevel::Confidential)
        );
    }

    #[test]
    fn pii_detector_passport() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("passport_number"),
            Some(ClassificationLevel::Restricted)
        );
    }

    #[test]
    fn pii_detector_drivers_license() {
        let detector = FieldNamePiiDetector::new();
        assert_eq!(
            detector.classify("drivers_license"),
            Some(ClassificationLevel::Restricted)
        );
        assert_eq!(
            detector.classify("license_number"),
            Some(ClassificationLevel::Restricted)
        );
    }

    #[test]
    fn classification_level_serde_roundtrip() {
        let level = ClassificationLevel::Restricted;
        let json = serde_json::to_string(&level).unwrap();
        let deser: ClassificationLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, level);
    }

    #[test]
    fn erasure_request_with_reason() {
        let req = ErasureRequest::new("r1".into(), "user1".into(), vec!["all".into()])
            .with_reason("GDPR Article 17");
        assert_eq!(req.reason.as_deref(), Some("GDPR Article 17"));
    }

    #[test]
    fn in_memory_erasure_sink_verify_post_erasure() {
        let sink = InMemoryErasureSink::new();
        sink.add_records("user_1", vec!["rec1".into(), "rec2".into()]);

        let req = ErasureRequest::new("r1".into(), "user_1".into(), vec!["all".into()]);
        let result = sink.erase(&req);
        assert_eq!(result.records_erased, 2);

        // Second erasure should find nothing
        let result2 = sink.erase(&req);
        assert_eq!(result2.records_erased, 0);
    }

    #[test]
    fn pii_detector_total_categories() {
        let detector = FieldNamePiiDetector::new();
        // 9 original + 4 Korean = 13 categories
        assert_eq!(detector.patterns.len(), 13);
    }

    #[test]
    fn sensitive_into_inner_returns_value() {
        let s = Sensitive::new(42);
        assert_eq!(s.into_inner(), 42);
    }

    #[cfg(feature = "xor-demo")]
    #[test]
    #[allow(deprecated)]
    fn xor_encryption_different_keys_differ() {
        let hook1 = XorEncryption::new(0x42);
        let hook2 = XorEncryption::new(0xFF);
        let data = b"test";
        assert_ne!(hook1.encrypt(data), hook2.encrypt(data));
    }
}
