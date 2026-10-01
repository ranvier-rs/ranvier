// Actual complex subscription policy export. Static Tower fragment is a separate gate.
use ranvier::prelude::*;
#[rustfmt::skip]
mod public_subscription {
use super::*;
// Complex case: Transition (visualized)
// Trusted ingress inserts this capability after credential verification.
// This example does not authenticate a client-supplied header itself.
#[derive(Clone)]
struct VerifiedPrincipal { subject: String, roles: Vec<String> }
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct AuthenticatedRequest { body: String, subject: String, roles: Vec<String> }
// Local authoritative snapshot for this example; revocation needs a production ledger.
struct AccessPolicy { active_subjects: std::collections::HashSet<String>, required_role: String }
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum AccessError { Unauthenticated, SubscriptionRequired, Forbidden }

#[transition(resources = AccessPolicy)]
async fn read_principal(input: String, bus: &mut Bus) -> Outcome<AuthenticatedRequest, AccessError> {
    match bus.read::<VerifiedPrincipal>() {
        Some(principal) if !principal.subject.trim().is_empty() => Outcome::Next(AuthenticatedRequest {
            body: input, subject: principal.subject.clone(), roles: principal.roles.clone(),
        }),
        _ => Outcome::Fault(AccessError::Unauthenticated),
    }
}
#[transition]
async fn check_subscription(input: AuthenticatedRequest, policy: &AccessPolicy) -> Outcome<AuthenticatedRequest, AccessError> {
    if !policy.active_subjects.contains(&input.subject) { return Outcome::Fault(AccessError::SubscriptionRequired); }
    Outcome::Next(input)
}
#[transition]
async fn require_role(input: AuthenticatedRequest, policy: &AccessPolicy) -> Outcome<AuthenticatedRequest, AccessError> {
    if policy.required_role.trim().is_empty() || !input.roles.iter().any(|role| role == &policy.required_role) {
        return Outcome::Fault(AccessError::Forbidden);
    }
    Outcome::Next(input)
}
#[transition(resources = AccessPolicy)]
async fn handle_request(input: AuthenticatedRequest) -> Outcome<String, AccessError> {
    Outcome::Next(input.body)
}
#[tokio::test]
async fn actual_subscription_chain_denies_missing_inactive_and_wrong_roles() {

let axon = Axon::<String, String, AccessError, AccessPolicy>::new("subscribed-request")
    .then(read_principal)
    .then(check_subscription)
    .then(require_role)
    .then(handle_request);
    for (subject, roles, active, required_role, expected) in [
        (None, vec![], true, "admin", Some(AccessError::Unauthenticated)),
        (Some(" "), vec!["admin"], true, "admin", Some(AccessError::Unauthenticated)),
        (Some("alice"), vec!["admin"], false, "admin", Some(AccessError::SubscriptionRequired)),
        (Some("alice"), vec!["reader"], true, "admin", Some(AccessError::Forbidden)),
        (Some("alice"), vec!["Admin"], true, "admin", Some(AccessError::Forbidden)),
        (Some("alice"), vec!["admin"], true, "", Some(AccessError::Forbidden)),
        (Some("alice"), vec![" "], true, " ", Some(AccessError::Forbidden)),
        (Some("alice"), vec!["admin"], true, "admin", None),
    ] {
        let mut bus = Bus::new();
        if let Some(subject) = subject { bus.insert(VerifiedPrincipal { subject: subject.into(), roles: roles.into_iter().map(String::from).collect() }); }
        let policy = AccessPolicy { active_subjects: if active { ["alice".into()].into() } else { Default::default() }, required_role: required_role.into() };
        let report = axon.execute("private-request-sentinel".into(), &policy, &mut bus).await;
        match report {
            ExecutionReport::Unpersisted { result: Outcome::Next(body), .. } => { assert!(expected.is_none()); assert_eq!(body, "private-request-sentinel"); }
            ExecutionReport::Unpersisted { result: Outcome::Fault(error), .. } => { assert_eq!(Some(error), expected); }
            _ => panic!("unexpected policy report"),
        }
    }
}
}
