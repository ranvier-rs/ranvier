#[path = "../src/auth.rs"]
mod auth;
use ranvier_core::iam::{IamError, IamVerifier};

#[tokio::test]
async fn verifier_configuration_and_safe_errors() {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    assert!(auth::JwtVerifier::from_secret(b"").is_err());
    let verifier = auth::JwtVerifier::from_secret(b"owned-fixture-key").unwrap();
    let token = verifier
        .issue_token("fixture", vec!["admin".into()])
        .unwrap();
    let identity = verifier.verify(&token).await.unwrap();
    assert_eq!(identity.subject, "fixture");
    for (algorithm, exp) in [(Algorithm::HS256, 1), (Algorithm::HS384, 4102444800)] {
        let rejected = encode(
            &Header::new(algorithm),
            &auth::Claims {
                sub: "private-subject-marker".into(),
                roles: vec!["admin".into()],
                exp,
            },
            &EncodingKey::from_secret(b"owned-fixture-key"),
        )
        .unwrap();
        assert!(matches!(verifier.verify(&rejected).await,
            Err(IamError::InvalidToken(error)) if error == "Authentication failed"));
    }
    let wrong = auth::JwtVerifier::from_secret(b"different-key").unwrap();
    for (owner, token) in [
        (&wrong, token.as_str()),
        (&verifier, "private-invalid-token"),
    ] {
        assert!(
            matches!(owner.verify(token).await, Err(IamError::InvalidToken(error)) if error == "Authentication failed")
        );
    }
}

#[test]
fn demo_process_requires_configuration_and_redacts_credentials() {
    let binary = env!("CARGO_BIN_EXE_auth-jwt-role-demo");
    let missing = std::process::Command::new(binary)
        .env_remove("JWT_SECRET")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let error = String::from_utf8(missing.stderr).unwrap();
    assert!(error.contains("JWT_SECRET configuration is unavailable"));
    assert!(!error.contains("panicked"));
    let owned = std::process::Command::new(binary)
        .env("JWT_SECRET", "owned-process-secret-marker")
        .output()
        .unwrap();
    assert!(owned.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8(owned.stdout).unwrap(),
        String::from_utf8(owned.stderr).unwrap()
    );
    assert!(output.contains("done"));
    for private in [
        "owned-process-secret-marker",
        "eyJ",
        "invalid.jwt.token",
        "Payload:",
        "JWT decode failed:",
    ] {
        assert!(
            !output.contains(private),
            "private diagnostic marker {private}"
        );
    }
}
