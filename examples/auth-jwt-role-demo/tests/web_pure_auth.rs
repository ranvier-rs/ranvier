// Actual public JWT example, tested against existing auth-jwt-role-demo dependencies.
#[tokio::test]
async fn public_typed_jwt_chain() {
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::Axon;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use jsonwebtoken::{decode, DecodingKey, Validation};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthContext {
    user_id: String,
    roles: Vec<String>,
    exp: usize,
}

// Configure the key, algorithm, issuer/audience and expiry policy at startup.
// The ingress adapter extracts the bearer token as String.
struct AuthResources {
    key: DecodingKey,
    validation: Validation,
    required_role: String,
}

#[derive(Clone)]
struct Authenticate;
#[async_trait]
impl Transition<String, AuthContext> for Authenticate {
    type Error = String;
    type Resources = AuthResources;
    async fn run(&self, token: String, res: &AuthResources, _: &mut Bus) -> Outcome<AuthContext, String> {
        match decode::<AuthContext>(&token, &res.key, &res.validation) {
            Ok(data) => Outcome::Next(data.claims),
            Err(_) => Outcome::Fault("Authentication failed".into()),
        }
    }
}

#[derive(Clone)]
struct Authorize;
#[async_trait]
impl Transition<AuthContext, AuthContext> for Authorize {
    type Error = String;
    type Resources = AuthResources;
    async fn run(&self, auth: AuthContext, res: &AuthResources, _: &mut Bus) -> Outcome<AuthContext, String> {
        if auth.roles.iter().any(|role| role == &res.required_role) {
            Outcome::Next(auth)
        } else {
            Outcome::Fault("Access denied".into())
        }
    }
}

#[derive(Clone)]
struct ProtectedHandler;
#[async_trait]
impl Transition<AuthContext, String> for ProtectedHandler {
    type Error = String;
    type Resources = AuthResources;
    async fn run(&self, auth: AuthContext, _: &AuthResources, _: &mut Bus) -> Outcome<String, String> {
        Outcome::Next(format!("Hello, {}", auth.user_id))
    }
}

let admin_axon = Axon::<String, String, String, AuthResources>::new("admin")
    .then(Authenticate)
    .then(Authorize)
    .then(ProtectedHandler);
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use ranvier_runtime::ExecutionReport;
    let secret = b"owned-fixture-key-not-production";
    let resources = AuthResources { key: DecodingKey::from_secret(secret),
        validation: Validation::new(Algorithm::HS256), required_role: "admin".into() };
    for (roles, exp, signing_key, expected) in [
        (vec!["admin".to_string()], 4102444800usize, secret.as_slice(), "Hello, fixture-user"),
        (vec!["reader".to_string()], 4102444800usize, secret.as_slice(), "Access denied"),
        (vec!["admin".to_string()], 1usize, secret.as_slice(), "Authentication failed"),
        (vec!["admin".to_string()], 4102444800usize, b"other-fixture-key".as_slice(), "Authentication failed"),
    ] {
        let claims = AuthContext { user_id: "fixture-user".into(), roles, exp };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(signing_key)).unwrap();
        let report = admin_axon.execute(token, &resources, &mut Bus::new()).await;
        match report {
            ExecutionReport::Unpersisted { result: Outcome::Next(value), .. } => { assert_eq!(expected, "Hello, fixture-user"); assert_eq!(value, expected); }
            ExecutionReport::Unpersisted { result: Outcome::Fault(error), .. } => { assert_ne!(expected, "Hello, fixture-user"); assert_eq!(error, expected); }
            _ => panic!("unexpected auth report"),
        }
    }
    let report = admin_axon.execute("invalid-private-token".into(), &resources, &mut Bus::new()).await;
    assert!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Fault(ref error), .. } if error == "Authentication failed"));
}
