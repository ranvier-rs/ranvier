// Actual philosophy export strings; models/test invocations are explicit fixture context.
use ranvier::prelude::*;
#[derive(serde::Serialize, serde::Deserialize)]
struct Request { body: String }
#[derive(serde::Serialize, serde::Deserialize)]
struct ValidRequest { body: String }
struct AuthContext { roles: Vec<String> }
#[tokio::test]
async fn typed_transition_flow() {
use ranvier::prelude::*;

#[transition]
async fn validate_input(req: Request) -> Outcome<ValidRequest, String> {
    if req.body.is_empty() {
        return Outcome::Fault("Request body is empty".into());
    }
    Outcome::Next(ValidRequest { body: req.body })
}

#[transition]
async fn process(input: ValidRequest) -> Outcome<String, String> {
    Outcome::Next(input.body.to_uppercase())
}

// Compose:
let axon = Axon::<Request, Request, String>::new("process-request")
    .then(validate_input)
    .then(process);
    for (body, valid) in [("hello", true), ("", false)] {
        let report = axon.execute(Request { body: body.into() }, &(), &mut Bus::new()).await;
        match report {
            ExecutionReport::Unpersisted { result: Outcome::Next(value), .. } => { assert!(valid); assert_eq!(value, "HELLO"); }
            ExecutionReport::Unpersisted { result: Outcome::Fault(error), .. } => { assert!(!valid); assert_eq!(error, "Request body is empty"); }
            _ => panic!("unexpected report"),
        }
    }
}
#[tokio::test]
async fn outcome_typed_handoff() {
// Transition returns Outcome
#[transition]
async fn normalize(input: String) -> Outcome<String, String> {
    if input.trim().is_empty() {
        return Outcome::Fault("Input is empty".into());
    }
    Outcome::Next(input.trim().to_string())
}

// Next carries the typed value to the following transition.
#[transition]
async fn format_output(input: String) -> Outcome<String, String> {
    Outcome::Next(format!("Hello, {input}!"))
}
let axon = Axon::<String, String, String>::new("greeting")
    .then(normalize)
    .then(format_output);
    let report = axon.execute(" World ".into(), &(), &mut Bus::new()).await;
    assert!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Next(ref value), .. } if value == "Hello, World!"));
    let report = axon.execute(" ".into(), &(), &mut Bus::new()).await;
    assert!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Fault(ref error), .. } if error == "Input is empty"));
}
#[tokio::test]
async fn bus_context_is_explicit_and_denies_missing_role() {
// A trusted ingress adapter supplies AuthContext after authentication.
#[transition]
async fn authorize(input: String, bus: &mut Bus) -> Outcome<String, String> {
    match bus.read::<AuthContext>() {
        Some(auth) if auth.roles.iter().any(|role| role == "admin") => Outcome::Next(input),
        _ => Outcome::Fault("Access denied".into()),
    }
}
// Domain input is a typed value; capabilities are explicitly read from Bus.
    let axon = Axon::<String, String, String>::new("authorize").then(authorize);
    for roles in [None, Some(vec!["reader".to_string()]), Some(vec!["admin".to_string()])] {
        let allowed = roles.as_ref().is_some_and(|roles| roles.iter().any(|role| role == "admin"));
        let mut bus = Bus::new();
        if let Some(roles) = roles { bus.insert(AuthContext { roles }); }
        let report = axon.execute("payload".into(), &(), &mut bus).await;
        assert_eq!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Next(_), .. }), allowed);
    }
}
#[tokio::test]
async fn schematic_is_structure_not_executor() -> Result<(), serde_json::Error> {
use ranvier::prelude::*;

#[transition]
async fn validate_input(req: Request) -> Outcome<ValidRequest, String> {
    if req.body.is_empty() {
        return Outcome::Fault("Request body is empty".into());
    }
    Outcome::Next(ValidRequest { body: req.body })
}

#[transition]
async fn process(input: ValidRequest) -> Outcome<String, String> {
    Outcome::Next(input.body.to_uppercase())
}


    let request = Request { body: "hello".into() };
let axon = Axon::<Request, Request, String>::new("process-request")
    .then(validate_input)
    .then(process);
let mut bus = Bus::new();
let report = axon.execute(request, &(), &mut bus).await;
// Inspect the local execution report before retrying.
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(output), .. } => println!("{output}"),
    _ => eprintln!("Execution requires review before retry"),
}
// Schematic describes structure; execution belongs to Axon.
let json = serde_json::to_string_pretty(axon.schematic())?;
println!("{json}");
    let structure: serde_json::Value = serde_json::from_str(&json)?;
    assert_eq!(structure["name"], "process-request");
    assert_eq!(structure["nodes"].as_array().unwrap().len(), 3);
    Ok(())
}
