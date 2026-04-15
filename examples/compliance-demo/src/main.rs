//! # Compliance Demo
//!
//! Demonstrates Ranvier's data compliance utilities for handling PII (Personally
//! Identifiable Information) in GDPR/HIPAA-regulated environments.
//!
//! ## Run
//! ```bash
//! cargo run -p compliance-demo
//! ```
//!
//! ## Key APIs
//! - `Sensitive<T>` — wrapper that redacts data in Debug/Display but preserves in Serialize
//! - `Redact` trait — custom redaction strategy
//! - `PiiDetector` trait — automatic PII detection interface
//!
//! ## Behavior
//! - `Debug` / `Display` output: `[REDACTED]` (safe for logs)
//! - `Serialize` output: actual value (assumes TLS-encrypted channel)
//! - `expose()`: explicit unwrap for authorized access

use ranvier_compliance::{
    ErasureRequest, ErasureSink, InMemoryErasureSink, PiiDetector, Redact, Sensitive,
};
use serde::{Deserialize, Serialize};

// ── Domain types with sensitive fields ────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct UserProfile {
    id: String,
    name: String,
    email: Sensitive<String>,
    ssn: Sensitive<String>,
    phone: Sensitive<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaymentInfo {
    transaction_id: String,
    card_number: Sensitive<String>,
    cvv: Sensitive<String>,
    amount: f64,
    currency: String,
}

// ── Custom Redact implementation ──────────────────────────────────────────

/// Demonstrates custom redaction strategy for credit card numbers.
/// Shows only last 4 digits: "4532-1111-2222-3333" -> "****-****-****-3333"
struct CardRedactor;

impl Redact for CardRedactor {
    fn redact(&self) -> String {
        "****-****-****-XXXX".to_string()
    }
}

/// Demonstrates a pattern-based PII detector.
struct BasicPiiDetector;

impl PiiDetector for BasicPiiDetector {
    fn contains_pii(&self, text: &str) -> bool {
        // Check for common PII patterns
        let patterns = [
            // Email pattern
            text.contains('@') && text.contains('.'),
            // SSN pattern (XXX-XX-XXXX)
            text.len() == 11
                && text.chars().nth(3) == Some('-')
                && text.chars().nth(6) == Some('-'),
            // Phone pattern (digits with dashes or spaces)
            text.len() >= 10 && text.chars().filter(|c| c.is_ascii_digit()).count() >= 10,
            // Credit card pattern (16+ digits)
            text.chars().filter(|c| c.is_ascii_digit()).count() >= 15,
        ];
        patterns.iter().any(|&p| p)
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

fn main() {
    println!("Compliance Demo");
    println!("================");
    println!();

    // ── 1. Sensitive<T> basics ──────────────────────────────────
    println!("1. Sensitive<T> — Debug/Display vs Serialize");
    println!("   -----------------------------------------");

    let user = UserProfile {
        id: "usr-42".into(),
        name: "Jane Doe".into(),
        email: Sensitive::new("jane.doe@example.com".into()),
        ssn: Sensitive::new("123-45-6789".into()),
        phone: Sensitive::new("+1-555-0123-4567".into()),
    };

    // Debug output: PII fields show [REDACTED]
    println!("   Debug output (safe for logs):");
    println!("     {user:?}");
    println!();

    // Display individual fields
    println!("   Display individual fields:");
    println!("     email = {}", user.email); // [REDACTED]
    println!("     ssn   = {}", user.ssn); // [REDACTED]
    println!("     phone = {}", user.phone); // [REDACTED]
    println!();

    // Serialize output: actual values transmitted (assumes TLS)
    println!("   Serialize output (for secure transmission):");
    let json = serde_json::to_string_pretty(&user).unwrap();
    println!("     {json}");
    println!();

    // Explicit access via expose()
    println!("   Explicit access via .expose():");
    println!("     email = {}", user.email.expose());
    println!();

    // ── 2. Payment data handling ────────────────────────────────
    println!("2. Payment data with Sensitive<T>");
    println!("   ------------------------------");

    let payment = PaymentInfo {
        transaction_id: "txn-2026-0001".into(),
        card_number: Sensitive::new("4532-1111-2222-3333".into()),
        cvv: Sensitive::new("123".into()),
        amount: 299.99,
        currency: "USD".into(),
    };

    println!("   Debug (safe): {payment:?}");
    println!();

    // ── 3. Deserialization round-trip ────────────────────────────
    println!("3. Serialization round-trip");
    println!("   -----------------------");

    let serialized = serde_json::to_string(&payment).unwrap();
    println!("   Serialized: {serialized}");

    let deserialized: PaymentInfo = serde_json::from_str(&serialized).unwrap();
    println!("   Deserialized card (debug): {}", deserialized.card_number);
    println!(
        "   Deserialized card (expose): {}",
        deserialized.card_number.expose()
    );
    println!();

    // ── 4. Custom Redact trait ───────────────────────────────────
    println!("4. Custom Redact trait");
    println!("   -------------------");

    let redactor = CardRedactor;
    println!("   CardRedactor output: {}", redactor.redact());
    println!();

    // ── 5. PII Detection ────────────────────────────────────────
    println!("5. PII Detection");
    println!("   -------------");

    let detector = BasicPiiDetector;
    let test_cases = [
        ("jane@example.com", true),
        ("123-45-6789", true),
        ("+1-555-0123-4567", true),
        ("4532111122223333", true),
        ("Hello, world!", false),
        ("order-12345", false),
    ];

    for (text, expected) in test_cases {
        let detected = detector.contains_pii(text);
        let status = if detected == expected {
            "OK"
        } else {
            "MISMATCH"
        };
        println!("   [{status}] \"{text}\" -> pii={detected} (expected={expected})");
    }
    println!();

    // ── 6. GDPR-aware logging pattern ───────────────────────────
    println!("6. GDPR-aware logging pattern");
    println!("   --------------------------");
    println!("   Logging user activity without exposing PII:");
    println!(
        "   [LOG] User {} performed action on resource (email: {})",
        user.id, user.email
    );
    println!("   -> PII fields automatically redacted in log output.");
    println!();
    println!("   Transmitting to secure service (serialized):");
    println!("   [SEND] {}", serde_json::to_string(&user.email).unwrap());
    println!("   -> Actual value transmitted over TLS-encrypted channel.");
    println!();

    // ── 7. GDPR Right-to-Erasure (Article 17) ───────────────────
    println!("7. GDPR Right-to-Erasure (Article 17)");
    println!("   -----------------------------------");

    let erasure_sink = InMemoryErasureSink::new();
    erasure_sink.add_records(
        "user-42",
        vec![
            "profile:jane.doe@example.com".into(),
            "order:ORD-001".into(),
            "payment:txn-2026-0001".into(),
        ],
    );

    let request = ErasureRequest::new(
        "erasure-req-001".into(),
        "user-42".into(),
        vec!["all".into()],
    )
    .with_reason("GDPR Article 17 — right to erasure");

    println!("   Request: {:?}", request.id);
    println!("   Subject: {}", request.subject);
    println!("   Reason:  {:?}", request.reason);

    let result = erasure_sink.erase(&request);
    println!(
        "   Result:  success={}, records_erased={}",
        result.success, result.records_erased
    );

    // Verify erasure
    let verify = erasure_sink.erase(&request);
    println!(
        "   Verify:  records_erased={} (should be 0 after erasure)",
        verify.records_erased
    );
}
