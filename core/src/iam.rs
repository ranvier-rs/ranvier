//! Enterprise IAM (Identity and Access Management) at the Schematic boundary.
//!
//! Provides protocol-agnostic IAM types that Axon checks at `execute()` time:
//!
//! * [`IamPolicy`] — what level of identity is required
//! * [`IamVerifier`] — how tokens are verified (OIDC/JWKS, HS256, custom)
//! * [`IamIdentity`] — the verified identity result
//! * [`IamToken`] — Bus-injectable bearer token
//!
//! The HTTP layer (or test harness) inserts an [`IamToken`] into the Bus.
//! The Axon boundary reads the token, calls the [`IamVerifier`], checks
//! the [`IamPolicy`], and injects the resulting [`IamIdentity`] for
//! downstream Transitions to consume.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// ── Policy ─────────────────────────────────────────────────────

/// IAM policy enforced at the Axon/Schematic execution boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum IamPolicy {
    /// No identity verification required.
    None,
    /// Any valid, verified identity is sufficient.
    RequireIdentity,
    /// Identity must possess the specified role.
    RequireRole(String),
    /// Identity must possess ALL specified claims.
    RequireClaims(Vec<String>),
}

impl Default for IamPolicy {
    fn default() -> Self {
        Self::None
    }
}

// ── Identity ───────────────────────────────────────────────────

/// Verified identity produced by an [`IamVerifier`].
///
/// Inserted into the Bus after successful verification so Transition
/// code can read it via `bus.read::<IamIdentity>()`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IamIdentity {
    pub subject: String,
    pub issuer: Option<String>,
    pub roles: Vec<String>,
    pub claims: HashMap<String, serde_json::Value>,
}

impl IamIdentity {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            issuer: None,
            roles: Vec::new(),
            claims: HashMap::new(),
        }
    }

    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
        self
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    pub fn has_claim(&self, claim: &str) -> bool {
        self.claims.contains_key(claim)
    }
}

// ── Error ──────────────────────────────────────────────────────

/// Error returned when IAM verification or policy enforcement fails.
#[derive(Debug)]
pub enum IamError {
    /// No token was provided but the policy requires one.
    MissingToken,
    /// Token is invalid or malformed.
    InvalidToken(String),
    /// Token has expired.
    Expired,
    /// Identity lacks the required role.
    InsufficientRole {
        required: String,
        found: Vec<String>,
    },
    /// Identity is missing one or more required claims.
    MissingClaims(Vec<String>),
}

impl fmt::Display for IamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToken => write!(f, "IAM: no token provided"),
            Self::InvalidToken(msg) => write!(f, "IAM: invalid token: {}", msg),
            Self::Expired => write!(f, "IAM: token expired"),
            Self::InsufficientRole { required, found } => {
                write!(f, "IAM: required role '{}', found {:?}", required, found)
            }
            Self::MissingClaims(claims) => write!(f, "IAM: missing claims: {:?}", claims),
        }
    }
}

impl std::error::Error for IamError {}

// ── Verifier trait ─────────────────────────────────────────────

/// Trait for verifying bearer tokens at the Schematic/Axon boundary.
///
/// Implementations may use:
/// - JWKS key sets with RS256/ES256 (OIDC / OAuth2)
/// - Shared secrets with HS256
/// - External identity providers
/// - Custom verification logic
#[async_trait]
pub trait IamVerifier: Send + Sync {
    /// Verify the raw token string and return the verified identity.
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError>;
}

// ── Bus-injectable types ───────────────────────────────────────

/// Bearer token injected into the Bus by the HTTP layer (or test harness).
///
/// The Axon boundary reads this and feeds it to the [`IamVerifier`].
#[derive(Clone, Debug)]
pub struct IamToken(pub String);

/// Bus-injectable handle containing the verifier and policy.
///
/// Attach to an Axon via `with_iam()` or inject directly into the Bus.
#[derive(Clone)]
pub struct IamHandle {
    pub policy: IamPolicy,
    pub verifier: Arc<dyn IamVerifier>,
}

impl IamHandle {
    pub fn new(policy: IamPolicy, verifier: Arc<dyn IamVerifier>) -> Self {
        Self { policy, verifier }
    }
}

// ── Policy enforcement ─────────────────────────────────────────

/// Enforce [`IamPolicy`] against a verified [`IamIdentity`].
///
/// Returns `Ok(())` if the identity satisfies the policy, or an
/// appropriate [`IamError`] if not.
pub fn enforce_policy(policy: &IamPolicy, identity: &IamIdentity) -> Result<(), IamError> {
    match policy {
        IamPolicy::None => Ok(()),
        IamPolicy::RequireIdentity => Ok(()), // presence of identity is sufficient
        IamPolicy::RequireRole(role) => {
            if identity.has_role(role) {
                Ok(())
            } else {
                Err(IamError::InsufficientRole {
                    required: role.clone(),
                    found: identity.roles.clone(),
                })
            }
        }
        IamPolicy::RequireClaims(required) => {
            let missing: Vec<String> = required
                .iter()
                .filter(|c| !identity.has_claim(c))
                .cloned()
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(IamError::MissingClaims(missing))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_none_always_passes() {
        let id = IamIdentity::new("alice");
        assert!(enforce_policy(&IamPolicy::None, &id).is_ok());
    }

    #[test]
    fn policy_require_identity_passes_for_any_subject() {
        let id = IamIdentity::new("bob");
        assert!(enforce_policy(&IamPolicy::RequireIdentity, &id).is_ok());
    }

    #[test]
    fn policy_require_role_passes_when_present() {
        let id = IamIdentity::new("carol").with_role("admin");
        assert!(enforce_policy(&IamPolicy::RequireRole("admin".into()), &id).is_ok());
    }

    #[test]
    fn policy_require_role_fails_when_absent() {
        let id = IamIdentity::new("dave").with_role("user");
        let err = enforce_policy(&IamPolicy::RequireRole("admin".into()), &id).unwrap_err();
        assert!(matches!(err, IamError::InsufficientRole { .. }));
    }

    #[test]
    fn policy_require_claims_passes_when_all_present() {
        let id = IamIdentity::new("eve")
            .with_claim("email", serde_json::json!("eve@example.com"))
            .with_claim("org", serde_json::json!("acme"));
        assert!(enforce_policy(
            &IamPolicy::RequireClaims(vec!["email".into(), "org".into()]),
            &id
        )
        .is_ok());
    }

    #[test]
    fn policy_require_claims_fails_when_missing() {
        let id = IamIdentity::new("frank")
            .with_claim("email", serde_json::json!("frank@example.com"));
        let err = enforce_policy(
            &IamPolicy::RequireClaims(vec!["email".into(), "org".into()]),
            &id,
        )
        .unwrap_err();
        match err {
            IamError::MissingClaims(missing) => assert_eq!(missing, vec!["org".to_string()]),
            other => panic!("Expected MissingClaims, got {:?}", other),
        }
    }
}
