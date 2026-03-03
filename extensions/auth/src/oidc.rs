//! OIDC / OAuth2 token verifier using JWKS (RS256 / ES256).
//!
//! Implements [`IamVerifier`] from ranvier-core, providing enterprise-grade
//! token verification at the Schematic/Axon boundary.
//!
//! # Usage
//!
//! ```rust,ignore
//! use ranvier_auth::oidc::OidcVerifier;
//! use ranvier_core::iam::{IamHandle, IamPolicy};
//!
//! // Build verifier from a pre-loaded JWKS JSON string
//! let verifier = OidcVerifier::from_jwks_json(
//!     jwks_json,
//!     "https://accounts.example.com",  // expected issuer
//!     "my-api-audience",               // expected audience
//! ).unwrap();
//!
//! // Attach to an Axon
//! let axon = Axon::new("Protected")
//!     .with_iam(IamPolicy::RequireRole("admin".into()), verifier)
//!     .then(MyStep);
//! ```

use async_trait::async_trait;
use jsonwebtoken::{
    Algorithm, DecodingKey, TokenData, Validation,
    decode, decode_header,
    jwk::{self, JwkSet},
};
use ranvier_core::iam::{IamError, IamIdentity, IamVerifier};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// OIDC/OAuth2 token verifier backed by a JWKS key set.
///
/// Supports RS256 and ES256 algorithms commonly used by identity providers
/// (Auth0, Okta, Azure AD, Google, Keycloak, etc.).
#[derive(Clone)]
pub struct OidcVerifier {
    jwks: Arc<JwkSet>,
    issuer: Vec<String>,
    audience: Vec<String>,
}

/// Standard OIDC JWT claims we extract.
#[derive(Debug, Serialize, Deserialize)]
struct OidcClaims {
    /// Subject (user identifier)
    sub: String,
    /// Issuer
    #[serde(default)]
    iss: Option<String>,
    /// Audience (can be string or array)
    #[serde(default)]
    aud: Option<serde_json::Value>,
    /// Expiration time
    #[serde(default)]
    exp: Option<u64>,
    /// Roles — many providers use different claim names; we normalize
    #[serde(default)]
    roles: Vec<String>,
    /// Collect all other claims
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

impl OidcVerifier {
    /// Create a verifier from a pre-loaded JWKS key set.
    ///
    /// * `jwks` — The JSON Web Key Set (typically fetched from `{issuer}/.well-known/jwks.json`)
    /// * `issuer` — Expected `iss` claim (e.g. `"https://accounts.google.com"`)
    /// * `audience` — Expected `aud` claim (your API's client ID)
    pub fn new(jwks: JwkSet, issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            jwks: Arc::new(jwks),
            issuer: vec![issuer.into()],
            audience: vec![audience.into()],
        }
    }

    /// Create a verifier from raw JWKS JSON (convenience constructor).
    pub fn from_jwks_json(
        jwks_json: &str,
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> Result<Self, String> {
        let jwks: JwkSet =
            serde_json::from_str(jwks_json).map_err(|e| format!("Invalid JWKS JSON: {}", e))?;
        Ok(Self::new(jwks, issuer, audience))
    }

    /// Find the JWK matching the token's `kid` header and return the decoding key
    /// along with the algorithm determined from the key type.
    fn find_key(&self, kid: &str) -> Result<(DecodingKey, Algorithm), IamError> {
        let jwk = self
            .jwks
            .keys
            .iter()
            .find(|k| k.common.key_id.as_deref() == Some(kid))
            .ok_or_else(|| {
                IamError::InvalidToken(format!("No JWKS key found for kid '{}'", kid))
            })?;

        // Determine algorithm from key type
        let alg = match &jwk.algorithm {
            jwk::AlgorithmParameters::RSA(_) => Algorithm::RS256,
            jwk::AlgorithmParameters::EllipticCurve(_) => Algorithm::ES256,
            _ => {
                return Err(IamError::InvalidToken(
                    "Unsupported JWK key type (only RSA/EC supported)".into(),
                ))
            }
        };

        let key = DecodingKey::from_jwk(jwk)
            .map_err(|e| IamError::InvalidToken(format!("Bad JWK key: {}", e)))?;

        Ok((key, alg))
    }

    /// Build a `Validation` configured for the given algorithm.
    fn make_validation(&self, alg: Algorithm) -> Validation {
        let mut validation = Validation::new(alg);
        validation.set_audience(&self.audience);
        validation.set_issuer(&self.issuer);
        validation.validate_exp = true;
        validation
    }
}

#[async_trait]
impl IamVerifier for OidcVerifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        // 1. Decode header to find the kid
        let header = decode_header(token)
            .map_err(|e| IamError::InvalidToken(format!("Malformed JWT header: {}", e)))?;

        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| IamError::InvalidToken("JWT missing 'kid' header".into()))?;

        // 2. Find the matching JWKS key and determine algorithm
        let (decoding_key, alg) = self.find_key(kid)?;

        // 3. Decode and validate the token with the key-specific algorithm
        let validation = self.make_validation(alg);
        let token_data: TokenData<OidcClaims> =
            decode(token, &decoding_key, &validation).map_err(|e| {
                let msg = e.to_string();
                if msg.contains("ExpiredSignature") {
                    IamError::Expired
                } else {
                    IamError::InvalidToken(msg)
                }
            })?;

        let claims = token_data.claims;

        // 4. Build the IamIdentity
        // Normalize roles: check both `roles` claim and common provider-specific claims
        let mut roles = claims.roles;
        // Azure AD / Keycloak often use "realm_access.roles"
        if let Some(serde_json::Value::Object(realm)) = claims.extra.get("realm_access") {
            if let Some(serde_json::Value::Array(realm_roles)) = realm.get("roles") {
                for r in realm_roles {
                    if let serde_json::Value::String(s) = r {
                        if !roles.contains(s) {
                            roles.push(s.clone());
                        }
                    }
                }
            }
        }

        // Collect remaining extra claims (excluding internal fields we already processed)
        let mut extra_claims = claims.extra;
        extra_claims.remove("realm_access"); // already consumed above

        Ok(IamIdentity {
            subject: claims.sub,
            issuer: claims.iss,
            roles,
            claims: extra_claims,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};

    /// Load test RSA key material and return (encoding_key, jwks_json, kid).
    fn test_rsa_keys() -> (EncodingKey, String, String) {
        let rsa_private_pem = include_str!("../test_fixtures/test_rsa_private.pem");
        let jwks_json = include_str!("../test_fixtures/test_jwks.json");

        let encoding_key = EncodingKey::from_rsa_pem(rsa_private_pem.as_bytes())
            .expect("test RSA private key");

        (encoding_key, jwks_json.to_string(), "test-key-1".to_string())
    }

    fn make_oidc_token(
        encoding_key: &EncodingKey,
        kid: &str,
        sub: &str,
        iss: &str,
        aud: &str,
        roles: &[&str],
        expired: bool,
    ) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let exp = if expired { now - 3600 } else { now + 3600 };

        let claims = serde_json::json!({
            "sub": sub,
            "iss": iss,
            "aud": aud,
            "exp": exp,
            "iat": now,
            "roles": roles,
        });

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());

        jsonwebtoken::encode(&header, &claims, encoding_key).expect("encode JWT")
    }

    #[tokio::test]
    async fn oidc_verifier_validates_valid_rs256_token() {
        let (encoding_key, jwks_json, kid) = test_rsa_keys();
        let verifier =
            OidcVerifier::from_jwks_json(&jwks_json, "https://issuer.example.com", "my-api")
                .expect("build verifier");

        let token = make_oidc_token(
            &encoding_key,
            &kid,
            "user-123",
            "https://issuer.example.com",
            "my-api",
            &["admin", "user"],
            false,
        );

        let identity = verifier.verify(&token).await.expect("should verify");
        assert_eq!(identity.subject, "user-123");
        assert_eq!(identity.issuer.as_deref(), Some("https://issuer.example.com"));
        assert!(identity.has_role("admin"));
        assert!(identity.has_role("user"));
    }

    #[tokio::test]
    async fn oidc_verifier_rejects_expired_token() {
        let (encoding_key, jwks_json, kid) = test_rsa_keys();
        let verifier =
            OidcVerifier::from_jwks_json(&jwks_json, "https://issuer.example.com", "my-api")
                .expect("build verifier");

        let token = make_oidc_token(
            &encoding_key,
            &kid,
            "user-456",
            "https://issuer.example.com",
            "my-api",
            &[],
            true,
        );

        let err = verifier.verify(&token).await.unwrap_err();
        assert!(matches!(err, IamError::Expired));
    }

    #[tokio::test]
    async fn oidc_verifier_rejects_wrong_issuer() {
        let (encoding_key, jwks_json, kid) = test_rsa_keys();
        let verifier =
            OidcVerifier::from_jwks_json(&jwks_json, "https://issuer.example.com", "my-api")
                .expect("build verifier");

        let token = make_oidc_token(
            &encoding_key,
            &kid,
            "user-789",
            "https://evil.example.com",
            "my-api",
            &[],
            false,
        );

        let err = verifier.verify(&token).await.unwrap_err();
        assert!(matches!(err, IamError::InvalidToken(_)));
    }

    #[tokio::test]
    async fn oidc_verifier_rejects_wrong_audience() {
        let (encoding_key, jwks_json, kid) = test_rsa_keys();
        let verifier =
            OidcVerifier::from_jwks_json(&jwks_json, "https://issuer.example.com", "my-api")
                .expect("build verifier");

        let token = make_oidc_token(
            &encoding_key,
            &kid,
            "user-abc",
            "https://issuer.example.com",
            "wrong-audience",
            &[],
            false,
        );

        let err = verifier.verify(&token).await.unwrap_err();
        assert!(matches!(err, IamError::InvalidToken(_)));
    }

    #[tokio::test]
    async fn oidc_verifier_rejects_unknown_kid() {
        let (encoding_key, jwks_json, _kid) = test_rsa_keys();
        let verifier =
            OidcVerifier::from_jwks_json(&jwks_json, "https://issuer.example.com", "my-api")
                .expect("build verifier");

        let token = make_oidc_token(
            &encoding_key,
            "unknown-kid",
            "user-def",
            "https://issuer.example.com",
            "my-api",
            &[],
            false,
        );

        let err = verifier.verify(&token).await.unwrap_err();
        assert!(matches!(err, IamError::InvalidToken(_)));
    }
}
